use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, instrument, warn};

use crate::core::flv_parser::{FlvDemuxer, FlvTag};
use crate::core::output::FlvPacket;
use crate::egress::dispatcher::StreamDispatcher;
use crate::ingest::events::StreamMessage;
use crate::egress::rtmp_egress::RtmpEgressHandler;
use crate::ingest::rtmp_worker::RtmpWorker;
use crate::otlp::metrics;
use crate::server::contracts::StreamRegistry;
use crate::settings::EgressConfig;
use crate::settings::load_settings;
use tracing::Span;

#[derive(Debug)]
pub struct RtmpServer {
    config: EgressConfig,
    stream_registry: Arc<dyn StreamRegistry>,
    flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
    stream_msg_tx: mpsc::UnboundedSender<(StreamMessage, Span)>,
}

impl RtmpServer {
    pub fn new(
        stream_registry: Arc<dyn StreamRegistry>,
        flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
        stream_msg_tx: mpsc::UnboundedSender<(StreamMessage, Span)>,
    ) -> Self {
        let config = load_settings().egress.clone();
        Self {
            config,
            stream_registry,
            flv_packet_tx,
            stream_msg_tx,
        }
    }

    #[instrument(name = "server.rtmp.start", skip(self, flv_packet_rx, shutdown), fields(server.port = self.config.port))]
    pub async fn start(
        &self,
        flv_packet_rx: mpsc::UnboundedReceiver<FlvPacket>,
        mut shutdown: broadcast::Receiver<()>,
    ) -> Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", self.config.port)).await?;
        info!(port = self.config.port, app = %self.config.appname, "RTMP server listening");

        let dispatcher = StreamDispatcher::new();
        let mut shutdown_clone = shutdown.resubscribe();
        let dispatcher_clone = dispatcher.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = process_flv_packets(flv_packet_rx, dispatcher_clone) => {},
                _ = shutdown_clone.recv() => {
                    info!("RTMP FLV packet processor shutting down");
                }
            }
        });

        loop {
            tokio::select! {
                _ = shutdown.recv() => {
                    info!("RTMP server received shutdown signal");
                    break;
                }
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((socket, addr)) => {
                            debug!(remote_addr = %addr, "New RTMP connection");
                            let mut connection = RtmpConnection::new(
                                socket,
                                self.config.appname.clone(),
                                dispatcher.clone(),
                                self.stream_registry.clone(),
                                self.flv_packet_tx.clone(),
                                self.stream_msg_tx.clone(),
                            );

                            tokio::spawn(async move {
                                if let Err(e) = connection.run().await {
                                    error!(remote_addr = %addr, error = %e, "RTMP connection error");
                                }
                                debug!(remote_addr = %addr, "RTMP connection closed");
                            });
                        }
                        Err(e) => {
                            error!(error = %e, "Failed to accept RTMP connection");
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

struct RtmpConnection {
    socket: TcpStream,
    appname: String,
    session: Option<ServerSession>,
    ingest_handler: RtmpWorker,
    egress_handler: RtmpEgressHandler,
    _session_guard: metrics::MetricGuard,
}

impl RtmpConnection {
    fn new(
        socket: TcpStream,
        appname: String,
        dispatcher: StreamDispatcher,
        stream_registry: Arc<dyn StreamRegistry>,
        flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
        stream_msg_tx: mpsc::UnboundedSender<(StreamMessage, Span)>,
    ) -> Self {
        Self {
            socket,
            appname: appname.clone(),
            session: None,
            ingest_handler: RtmpWorker::new(
                appname,
                stream_registry.clone(),
                flv_packet_tx,
                stream_msg_tx,
            ),
            egress_handler: RtmpEgressHandler::new(dispatcher, stream_registry),
            _session_guard: metrics::MetricGuard::new(&metrics::get_metrics().rtmp_sessions, vec![]),
        }
    }

    fn session_mut(&mut self) -> Result<&mut ServerSession> {
        self.session
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("RTMP Session not initialized"))
    }

    async fn run(&mut self) -> Result<()> {
        let addr = self.socket.peer_addr().ok();

        if !self.perform_handshake().await? {
            info!(remote_addr = ?addr, "Handshake incomplete or failed, disconnecting");
            return Ok(());
        }

        let config = ServerSessionConfig::new();
        let (session, results) = ServerSession::new(config)?;
        self.session = Some(session);
        self.write_response(results).await?;

        let mut buffer = [0u8; 4096];
        loop {
            tokio::select! {
                n = self.socket.read(&mut buffer) => {
                    let n = n?;
                    if n == 0 {
                        self.ingest_handler.cleanup_disconnect();
                        break;
                    }

                    if let Err(e) = self.handle_input(&buffer[..n]).await {
                        warn!(remote_addr = ?addr, error = %e, "Failed to handle input");
                        self.ingest_handler.cleanup_disconnect();
                        break;
                    }
                }
                tag = self.egress_handler.wait_for_tag() => {
                    if let Ok(tag) = tag {
                        let session = self
                            .session
                            .as_mut()
                            .ok_or_else(|| anyhow::anyhow!("RTMP Session not initialized"))?;
                        let egress_handler = &mut self.egress_handler;
                        let socket = &mut self.socket;

                        if let Err(e) = egress_handler.handle_broadcast_tag(session, socket, tag).await {
                            warn!(remote_addr = ?addr, error = %e, "Failed to send tag to client");
                            self.ingest_handler.cleanup_disconnect();
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn perform_handshake(&mut self) -> Result<bool> {
        let mut buffer = [0u8; 1536];
        let mut handshake = Handshake::new(PeerType::Server);

        loop {
            let n = self.socket.read(&mut buffer).await?;
            if n == 0 {
                return Ok(false);
            }

            let (completed, resp) = match handshake.process_bytes(&buffer[..n]) {
                Ok(HandshakeProcessResult::InProgress { response_bytes }) => {
                    (false, response_bytes)
                }
                Ok(HandshakeProcessResult::Completed { response_bytes, .. }) => {
                    (true, response_bytes)
                }
                Err(e) => anyhow::bail!("Handshake error: {:?}", e),
            };

            if !resp.is_empty() {
                self.socket.write_all(&resp).await?;
            }

            if completed {
                return Ok(true);
            }
        }
    }

    async fn handle_input(&mut self, data: &[u8]) -> Result<()> {
        let results = {
            let session = self.session_mut()?;
            session.handle_input(data)?
        };

        for result in results {
            match result {
                ServerSessionResult::OutboundResponse(packet) => {
                    self.socket.write_all(&packet.bytes).await?;
                }
                ServerSessionResult::RaisedEvent(event) => {
                    self.handle_event(event).await?;
                }
                _ => {}
            }
        }

        Ok(())
    }

    async fn handle_event(&mut self, event: ServerSessionEvent) -> Result<()> {
        match event {
            ServerSessionEvent::ConnectionRequested { app_name, request_id } => {
                let res = if app_name == self.appname {
                    self.session_mut()?.accept_request(request_id)?
                } else {
                    self.session_mut()?.reject_request(
                        request_id,
                        "AppNotFound",
                        "Application not found",
                    )?
                };

                self.write_response(res).await?;
            }
            ServerSessionEvent::PlayStreamRequested {
                stream_key,
                request_id,
                stream_id,
                ..
            } => {
                let session = self
                    .session
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("RTMP Session not initialized"))?;
                let egress_handler = &mut self.egress_handler;
                let res = egress_handler
                    .on_play_requested(session, stream_key, request_id, stream_id)
                    .await?;
                self.write_response(res).await?;
            }
            ServerSessionEvent::PlayStreamFinished { .. } => {
                self.egress_handler.on_play_finished();
            }
            ServerSessionEvent::PublishStreamRequested {
                app_name,
                stream_key,
                request_id,
                ..
            } => {
                let session = self
                    .session
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("RTMP Session not initialized"))?;
                let ingest_handler = &mut self.ingest_handler;
                let res = ingest_handler
                    .on_publish_requested(session, app_name, stream_key, request_id)
                    .await?;
                self.write_response(res).await?;
            }
            ServerSessionEvent::PublishStreamFinished { stream_key, .. } => {
                self.ingest_handler.on_publish_finished(stream_key);
            }
            ServerSessionEvent::AudioDataReceived {
                stream_key,
                timestamp,
                data,
                ..
            } => {
                self.ingest_handler
                    .on_audio_data(stream_key, timestamp.value, data.as_ref());
            }
            ServerSessionEvent::VideoDataReceived {
                stream_key,
                timestamp,
                data,
                ..
            } => {
                self.ingest_handler
                    .on_video_data(stream_key, timestamp.value, data.as_ref());
            }
            _ => {}
        }

        Ok(())
    }

    async fn write_response(&mut self, results: Vec<ServerSessionResult>) -> Result<()> {
        for result in results {
            if let ServerSessionResult::OutboundResponse(packet) = result {
                self.socket.write_all(&packet.bytes).await?;
            }
        }
        Ok(())
    }
}

#[instrument(name = "server.rtmp.flv.process", skip(flv_rx, dispatcher))]
async fn process_flv_packets(
    mut flv_rx: mpsc::UnboundedReceiver<FlvPacket>,
    dispatcher: StreamDispatcher,
) {
    let mut demuxers: HashMap<String, FlvDemuxer> = HashMap::new();

    while let Some(packet) = flv_rx.recv().await {
        let live_id = packet.live_id().to_string();
        let demuxer = demuxers.entry(live_id.clone()).or_insert_with(FlvDemuxer::new);

        match packet {
            FlvPacket::Data { data, .. } => {
                demuxer.push_data(&data);
                while let Some(tag) = demuxer.next_tag() {
                    let tag = Arc::new(tag);
                    let state = dispatcher.stream(&live_id).await;

                    match tag.as_ref() {
                        FlvTag::Video { payload, .. } => {
                            if payload.len() > 1 && payload[1] == 0 {
                                *state.video_seq_header.write().await = Some(tag.clone());
                            }
                        }
                        FlvTag::Audio { payload, .. } => {
                            if payload.len() > 1 && payload[1] == 0 {
                                *state.audio_seq_header.write().await = Some(tag.clone());
                            }
                        }
                        FlvTag::ScriptData { .. } => {
                            *state.metadata.write().await = Some(tag.clone());
                        }
                    }

                    let _ = state.sender.send(tag);
                }
            }
            FlvPacket::EndOfStream { live_id } => {
                demuxers.remove(&live_id);
                dispatcher.remove_stream(&live_id).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use tokio::sync::mpsc;

    use crate::egress::dispatcher::StreamDispatcher;

    use super::{FlvPacket, process_flv_packets};

    fn flv_header() -> Vec<u8> {
        vec![
            0x46, 0x4C, 0x56, 0x01, 0x05, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00, 0x00,
        ]
    }

    fn make_tag(tag_type: u8, timestamp: u32, payload: &[u8]) -> Vec<u8> {
        let data_size = payload.len() as u32;
        let mut out = Vec::with_capacity(11 + payload.len() + 4);

        out.push(tag_type);
        out.push(((data_size >> 16) & 0xFF) as u8);
        out.push(((data_size >> 8) & 0xFF) as u8);
        out.push((data_size & 0xFF) as u8);

        out.push(((timestamp >> 16) & 0xFF) as u8);
        out.push(((timestamp >> 8) & 0xFF) as u8);
        out.push((timestamp & 0xFF) as u8);
        out.push(((timestamp >> 24) & 0xFF) as u8);

        out.extend_from_slice(&[0x00, 0x00, 0x00]);
        out.extend_from_slice(payload);

        let prev_size = 11 + data_size;
        out.push(((prev_size >> 24) & 0xFF) as u8);
        out.push(((prev_size >> 16) & 0xFF) as u8);
        out.push(((prev_size >> 8) & 0xFF) as u8);
        out.push((prev_size & 0xFF) as u8);

        out
    }

    #[tokio::test]
    async fn process_flv_packets_updates_headers_and_cleans_on_eos() {
        let dispatcher = StreamDispatcher::new();
        let (tx, rx) = mpsc::unbounded_channel();
        let (mut sub_rx, state) = dispatcher.subscribe("live_1").await;

        let worker_dispatcher = dispatcher.clone();
        let handle = tokio::spawn(async move {
            process_flv_packets(rx, worker_dispatcher).await;
        });

        let mut first_chunk = flv_header();
        first_chunk.extend_from_slice(&make_tag(8, 100, &[0xAF, 0x00]));
        tx.send(FlvPacket::Data {
            live_id: "live_1".to_string(),
            data: Bytes::from(first_chunk),
        })
        .expect("first flv packet should send");

        tx.send(FlvPacket::Data {
            live_id: "live_1".to_string(),
            data: Bytes::from(make_tag(9, 120, &[0x17, 0x00, 0x00])),
        })
        .expect("second flv packet should send");

        let _ = sub_rx.recv().await.expect("should receive first tag");
        let _ = sub_rx.recv().await.expect("should receive second tag");

        assert!(state.audio_seq_header.read().await.is_some());
        assert!(state.video_seq_header.read().await.is_some());

        tx.send(FlvPacket::EndOfStream {
            live_id: "live_1".to_string(),
        })
        .expect("eos should send");

        drop(tx);
        handle.await.expect("processor task should exit");

        let recreated = dispatcher.stream("live_1").await;
        assert!(recreated.audio_seq_header.read().await.is_none());
        assert!(recreated.video_seq_header.read().await.is_none());
        assert!(recreated.metadata.read().await.is_none());
    }
}
