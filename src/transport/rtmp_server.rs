use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument, warn};

use crate::config::EgressConfig;
use crate::contracts::StreamRegistry;
use crate::egress::dispatcher::StreamDispatcher;
use crate::egress::rtmp_egress::RtmpEgressHandler;
use crate::ingest::adapters;
use crate::ingest::adapters::rtmp::RtmpTag;
use crate::media::flv_parser::{FlvDemuxer, FlvTag};
use crate::media::output::FlvPacket;
use crate::telemetry::metrics;

#[derive(Debug)]
pub struct RtmpServer {
    config: EgressConfig,
    stream_registry: Arc<dyn StreamRegistry>,
}

impl RtmpServer {
    pub fn new(config: EgressConfig, stream_registry: Arc<dyn StreamRegistry>) -> Self {
        Self {
            config,
            stream_registry,
        }
    }

    #[instrument(name = "server.rtmp.start", skip(self, flv_packet_rx, shutdown), fields(server.port = self.config.port))]
    pub async fn start(
        &self,
        flv_packet_rx: mpsc::UnboundedReceiver<FlvPacket>,
        shutdown: CancellationToken,
    ) -> Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", self.config.port)).await?;
        info!(port = self.config.port, app = %self.config.appname, "RTMP server listening");

        let dispatcher = StreamDispatcher::new();
        let shutdown_clone = shutdown.clone();
        let dispatcher_clone = dispatcher.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = process_flv_packets(flv_packet_rx, dispatcher_clone) => {},
                _ = shutdown_clone.cancelled() => {
                    info!("RTMP FLV packet processor shutting down");
                }
            }
        });

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
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
    stream_registry: Arc<dyn StreamRegistry>,
    rtmp_tx: Option<mpsc::UnboundedSender<RtmpTag>>,
    egress_handler: RtmpEgressHandler,
    _session_guard: metrics::MetricGuard,
}

impl RtmpConnection {
    fn new(
        socket: TcpStream,
        appname: String,
        dispatcher: StreamDispatcher,
        stream_registry: Arc<dyn StreamRegistry>,
    ) -> Self {
        Self {
            socket,
            appname: appname.clone(),
            session: None,
            stream_registry: stream_registry.clone(),
            rtmp_tx: None,
            egress_handler: RtmpEgressHandler::new(dispatcher, stream_registry),
            _session_guard: metrics::MetricGuard::new(
                &metrics::get_metrics().rtmp_sessions,
                vec![],
            ),
        }
    }

    fn session_mut(&mut self) -> Result<&mut ServerSession> {
        self.session
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("RTMP Session not initialized"))
    }

    fn notify_publish_finished(&mut self) {
        if let Some(tx) = self.rtmp_tx.take() {
            let _ = tx.send(RtmpTag::PublishFinished);
        }
    }

    fn send_rtmp_tag(&self, tag: RtmpTag) {
        if let Some(tx) = &self.rtmp_tx {
            let _ = tx.send(tag);
        }
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
        let mut control_stop_tick = interval(Duration::from_millis(250));
        loop {
            tokio::select! {
                n = self.socket.read(&mut buffer) => {
                    let n = n?;
                    if n == 0 {
                        self.notify_publish_finished();
                        break;
                    }

                    if let Err(e) = self.handle_input(&buffer[..n]).await {
                        warn!(remote_addr = ?addr, error = %e, "Failed to handle input");
                        self.notify_publish_finished();
                        break;
                    }
                }
                tag = self.egress_handler.wait_for_tag() => {
                    match tag {
                        Ok(tag) => {
                            let session = self
                                .session
                                .as_mut()
                                .ok_or_else(|| anyhow::anyhow!("RTMP Session not initialized"))?;
                            let egress_handler = &mut self.egress_handler;
                            let socket = &mut self.socket;

                            if let Err(e) = egress_handler.handle_broadcast_tag(session, socket, tag).await {
                                warn!(remote_addr = ?addr, error = %e, "Failed to send tag to client");
                                self.notify_publish_finished();
                                break;
                            }
                        }
                        Err(e) => {
                            warn!(remote_addr = ?addr, error = %e, "Error receiving tag from egress handler");
                            self.notify_publish_finished();
                            break;
                        }
                    }
                }
                _ = control_stop_tick.tick() => {
                    if self.rtmp_tx.as_ref().is_some_and(|tx| tx.is_closed()) {
                        info!(remote_addr = ?addr, "RTMP ingest worker closed, terminating publisher connection");
                        self.rtmp_tx = None;
                        break;
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
            ServerSessionEvent::ConnectionRequested {
                app_name,
                request_id,
            } => {
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
                if app_name == self.appname {
                    if let Some(rtmp_tx) = self.stream_registry.get_rtmp_tx(&stream_key).await {
                        self.rtmp_tx = Some(rtmp_tx.clone());
                        let header = bytes::Bytes::from_static(&[
                            0x46, 0x4C, 0x56, 0x01, 0x05, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00,
                            0x00,
                        ]);
                        let _ = rtmp_tx.send(RtmpTag::Header { tag: header });
                        let res = session.accept_request(request_id)?;
                        self.write_response(res).await?;
                    } else {
                        let res = session.reject_request(
                            request_id,
                            "StreamNotFound",
                            "Stream not found",
                        )?;
                        self.write_response(res).await?;
                    }
                } else {
                    let res = session.reject_request(
                        request_id,
                        "AppNotFound",
                        "Application not found",
                    )?;
                    self.write_response(res).await?;
                }
            }
            ServerSessionEvent::PublishStreamFinished { .. } => {
                self.notify_publish_finished();
            }
            ServerSessionEvent::AudioDataReceived {
                timestamp, data, ..
            } => {
                let tag = adapters::rtmp::make_rtmp_tag(8, timestamp.value, data.as_ref());
                self.send_rtmp_tag(RtmpTag::Audio {
                    tag,
                    timestamp: timestamp.value,
                    data_len: data.len(),
                });
            }
            ServerSessionEvent::VideoDataReceived {
                timestamp, data, ..
            } => {
                let tag = adapters::rtmp::make_rtmp_tag(9, timestamp.value, data.as_ref());
                self.send_rtmp_tag(RtmpTag::Video {
                    tag,
                    timestamp: timestamp.value,
                    payload: data.clone(),
                });
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
        let demuxer = demuxers
            .entry(live_id.clone())
            .or_insert_with(FlvDemuxer::new);

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
