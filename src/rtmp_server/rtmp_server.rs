use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use log::{error, info};
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{
    ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
};
use rml_rtmp::time::RtmpTimestamp;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};

use super::dispatcher::{StreamDispatcher, StreamState};
use crate::core::flv_parser::{FlvDemuxer, FlvTag};
use crate::core::output::FlvPacket;

#[derive(Debug)]
pub struct RtmpServer {
    rtmp_port: u16,
    flv_packet_rx: Option<mpsc::Receiver<FlvPacket>>,
}

impl RtmpServer {
    pub fn new(rtmp_port: u16, flv_packet_rx: mpsc::Receiver<FlvPacket>) -> Self {
        Self {
            rtmp_port,
            flv_packet_rx: Some(flv_packet_rx),
        }
    }

    pub async fn run(mut self) -> Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", self.rtmp_port)).await?;
        info!("RTMP Server listening on 0.0.0.0:{}", self.rtmp_port);

        let dispatcher = StreamDispatcher::new();

        // Background Task: Receive packets from source, demux, and dispatch
        let flv_rx = self.flv_packet_rx.take().unwrap();

        tokio::spawn(process_flv_packets(flv_rx, dispatcher.clone()));

        loop {
            let (socket, addr) = listener.accept().await?;
            info!("New RTMP connection from {}", addr);

            let dispatcher = dispatcher.clone();
            tokio::spawn(async move {
                if let Err(e) = connection_handler(socket, dispatcher).await {
                    error!("RTMP connection error {}: {}", addr, e);
                }
            });
        }
    }
}

async fn process_flv_packets(mut flv_rx: mpsc::Receiver<FlvPacket>, dispatcher: StreamDispatcher) {
    let mut demuxers: HashMap<String, FlvDemuxer> = HashMap::new();

    while let Some(packet) = flv_rx.recv().await {
        let live_id = packet.live_id.clone();
        let demuxer = demuxers
            .entry(live_id.clone())
            .or_insert_with(FlvDemuxer::new);

        demuxer.push_data(&packet.data);

        while let Some(tag) = demuxer.next_tag() {
            let tag = Arc::new(tag);

            // Cache sequence headers
            let state = dispatcher
                .streams
                .get_or_insert_with(live_id.clone(), || StreamState::new())
                .await;

            match tag.as_ref() {
                FlvTag::Video { payload, .. } => {
                    if payload.len() > 1 && payload[1] == 0 {
                        // AVCPacketType == 0 (Sequence Header)
                        *state.video_seq_header.write().await = Some(tag.clone());
                    }
                }
                FlvTag::Audio { payload, .. } => {
                    if payload.len() > 1 && payload[1] == 0 {
                        // AACPacketType == 0 (Sequence Header)
                        *state.audio_seq_header.write().await = Some(tag.clone());
                    }
                }
                FlvTag::ScriptData { .. } => {
                    *state.metadata.write().await = Some(tag.clone());
                }
            }

            // Ignore error if no subscribers
            let _ = state.sender.send(tag);
        }
    }
}

async fn connection_handler(mut socket: TcpStream, dispatcher: StreamDispatcher) -> Result<()> {
    // Handshake
    if !perform_handshake(&mut socket).await? {
        info!("Handshake incomplete, disconnecting");
        return Ok(());
    }

    let config = ServerSessionConfig::new();
    let (mut session, mut results) = ServerSession::new(config)?;
    let mut active_stream_rx: Option<broadcast::Receiver<Arc<FlvTag>>> = None;
    let mut current_stream_id: u32 = 0;
    let mut stream_state: Option<StreamState> = None;
    let mut sent_headers = false;

    // Initial response
    write_response(&mut socket, results).await?;

    let mut buffer = [0u8; 4096];

    loop {
        tokio::select! {
            // Read from Socket
            n = socket.read(&mut buffer) => {
                let n = n?;
                if n == 0 { break; } // Client disconnected

                results = session.handle_input(&buffer[..n])?;
                for result in results {
                    match result {
                        ServerSessionResult::OutboundResponse(packet) => socket.write_all(&packet.bytes).await?,
                        ServerSessionResult::RaisedEvent(event) => {
                            handle_event(event, &mut socket, &mut session, &dispatcher, &mut active_stream_rx, &mut current_stream_id, &mut stream_state).await?;
                        },
                        _ => {} // Ignore unhandleable for now
                    }
                }
            }

            // Write to Socket (from Broadcast)
            Ok(tag) = recv_next_tag(&mut active_stream_rx) => {
                // Send cached headers first if not sent yet
                if !sent_headers {
                    send_cached_headers(&mut socket, &mut session, current_stream_id, &stream_state).await?;
                    sent_headers = true;
                }

                send_tag_to_socket(&mut socket, &mut session, current_stream_id, &tag).await?;
            }
        }
    }

    async fn recv_next_tag(
        rx: &mut Option<broadcast::Receiver<Arc<FlvTag>>>,
    ) -> Result<Arc<FlvTag>> {
        match rx {
            Some(rx) => rx.recv().await.map_err(|e| e.into()),
            None => std::future::pending().await,
        }
    }

    Ok(())
}

async fn send_tag_to_socket(
    socket: &mut TcpStream,
    session: &mut ServerSession,
    stream_id: u32,
    tag: &FlvTag,
) -> Result<()> {
    let packet = match tag {
        FlvTag::Audio { timestamp, payload } => session.send_audio_data(
            stream_id,
            payload.clone().into(),
            RtmpTimestamp::new(*timestamp),
            false,
        )?,
        FlvTag::Video {
            timestamp,
            payload,
            is_keyframe,
        } => session.send_video_data(
            stream_id,
            payload.clone().into(),
            RtmpTimestamp::new(*timestamp),
            !*is_keyframe,
        )?,
        FlvTag::ScriptData { .. } => return Ok(()),
    };
    socket.write_all(&packet.bytes).await?;
    Ok(())
}

async fn perform_handshake(socket: &mut TcpStream) -> Result<bool> {
    let mut buffer = [0u8; 1536];
    let mut handshake = Handshake::new(PeerType::Server);

    loop {
        let n = socket.read(&mut buffer).await?;
        if n == 0 {
            return Ok(false);
        }

        let (completed, resp) = match handshake.process_bytes(&buffer[..n]) {
            Ok(HandshakeProcessResult::InProgress { response_bytes }) => (false, response_bytes),
            Ok(HandshakeProcessResult::Completed { response_bytes, .. }) => (true, response_bytes),
            Err(e) => anyhow::bail!("Handshake error: {:?}", e),
        };

        if !resp.is_empty() {
            socket.write_all(&resp).await?;
        }
        if completed {
            return Ok(true);
        }
    }
}

async fn handle_event(
    event: ServerSessionEvent,
    socket: &mut TcpStream,
    session: &mut ServerSession,
    dispatcher: &StreamDispatcher,
    active_stream_rx: &mut Option<broadcast::Receiver<Arc<FlvTag>>>,
    current_stream_id: &mut u32,
    stream_state: &mut Option<StreamState>,
) -> Result<()> {
    match event {
        ServerSessionEvent::ConnectionRequested {
            app_name,
            request_id,
        } => {
            info!("Connection requested: {}", app_name);
            let res = session.accept_request(request_id)?;
            write_response(socket, res).await?;
        }
        ServerSessionEvent::PlayStreamRequested {
            stream_key,
            request_id,
            stream_id,
            ..
        } => {
            info!("Play requested: {} (ID: {})", stream_key, stream_id);
            *current_stream_id = stream_id;

            let (rx, state) = dispatcher.subscribe(&stream_key).await;
            *active_stream_rx = Some(rx);
            *stream_state = Some(state);

            let res = session.accept_request(request_id)?;
            write_response(socket, res).await?;
        }
        ServerSessionEvent::PlayStreamFinished { .. } => {
            *active_stream_rx = None;
            *stream_state = None;
        }
        _ => {}
    }
    Ok(())
}

async fn write_response(socket: &mut TcpStream, results: Vec<ServerSessionResult>) -> Result<()> {
    for result in results {
        if let ServerSessionResult::OutboundResponse(packet) = result {
            socket.write_all(&packet.bytes).await?;
        }
    }
    Ok(())
}

async fn send_cached_headers(
    socket: &mut TcpStream,
    session: &mut ServerSession,
    current_stream_id: u32,
    stream_state: &Option<StreamState>,
) -> Result<()> {
    if let Some(state) = stream_state {
        if let Some(v_seq) = state.video_seq_header.read().await.as_ref() {
            send_tag_to_socket(socket, session, current_stream_id, v_seq).await?;
        }
        if let Some(a_seq) = state.audio_seq_header.read().await.as_ref() {
            send_tag_to_socket(socket, session, current_stream_id, a_seq).await?;
        }
    }
    Ok(())
}
