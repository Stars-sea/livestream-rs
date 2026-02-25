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

use super::dispatcher::StreamDispatcher;
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
        let dispatcher_clone = dispatcher.clone();
        let mut flv_rx = self.flv_packet_rx.take().unwrap();

        tokio::spawn(async move {
            let mut demuxers: HashMap<String, FlvDemuxer> = HashMap::new();

            while let Some(packet) = flv_rx.recv().await {
                let live_id = packet.live_id.clone();
                let demuxer = demuxers
                    .entry(live_id.clone())
                    .or_insert_with(FlvDemuxer::new);

                demuxer.push_data(&packet.data);

                while let Some(tag) = demuxer.next_tag() {
                    if let Some(tx) = dispatcher_clone.stream(&live_id).await {
                        // Ignore error if no subscribers
                        let _ = tx.send(Arc::new(tag));
                    }
                    // If stream exists in demuxer but no one is subscribed in dispatcher,
                    // we currently drop the tags. This is fine for live streaming.
                }
            }
        });

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
                            handle_event(event, &mut socket, &mut session, &dispatcher, &mut active_stream_rx, &mut current_stream_id).await?;
                        },
                        _ => {} // Ignore unhandleable for now
                    }
                }
            }

            // Write to Socket (from Broadcast)
            Ok(tag) = async {
                if let Some(rx) = &mut active_stream_rx {
                    rx.recv().await.map_err(|_| anyhow::anyhow!("skipped"))
                } else {
                    std::future::pending().await
                }
            } => {
                let packet = match tag.as_ref() {
                    FlvTag::Audio { timestamp, payload } =>
                        session.send_audio_data(current_stream_id, payload.clone().into(), RtmpTimestamp::new(*timestamp), false)?,
                    FlvTag::Video { timestamp, payload, is_keyframe } =>
                        session.send_video_data(current_stream_id, payload.clone().into(), RtmpTimestamp::new(*timestamp), !*is_keyframe)?,
                    _ => continue,
                };
                socket.write_all(&packet.bytes).await?;
            }
        }
    }
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
            *active_stream_rx = Some(dispatcher.subscribe(&stream_key).await);

            let res = session.accept_request(request_id)?;
            write_response(socket, res).await?;
        }
        ServerSessionEvent::PlayStreamFinished { .. } => {
            *active_stream_rx = None;
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
