use std::sync::Arc;

use anyhow::Result;
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::*;
use rml_rtmp::time::RtmpTimestamp;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tracing::{info, warn};

use super::dispatcher::StreamDispatcher;
use super::dispatcher::StreamState;

use crate::core::flv_parser::FlvTag;
use crate::ingest::StreamManager;

pub(super) struct RtmpConnection {
    socket: TcpStream,
    dispatcher: StreamDispatcher,
    appname: String,
    session: Option<ServerSession>,
    active_stream_rx: Option<broadcast::Receiver<Arc<FlvTag>>>,
    current_stream_id: u32,
    stream_state: Option<StreamState>,
    sent_headers: bool,

    ingest_manager: Arc<StreamManager>,
}

impl RtmpConnection {
    pub fn new(
        socket: TcpStream,
        dispatcher: StreamDispatcher,
        appname: String,
        ingest_manager: Arc<StreamManager>,
    ) -> Self {
        Self {
            socket,
            dispatcher,
            appname,
            session: None,
            active_stream_rx: None,
            current_stream_id: 0,
            stream_state: None,
            sent_headers: false,
            ingest_manager,
        }
    }

    fn session_mut(&mut self) -> Result<&mut ServerSession> {
        self.session
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("RTMP Session not initialized"))
    }

    pub async fn run(&mut self) -> Result<()> {
        let addr = self.socket.peer_addr().ok();
        // Handshake
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
                // Read from Socket
                n = self.socket.read(&mut buffer) => {
                    let n = n?;
                    if n == 0 {
                        info!(remote_addr = ?addr, "Client disconnected normally");
                        break;
                    }
                    if let Err(e) = self.handle_input(&buffer[..n]).await {
                         warn!(remote_addr = ?addr, error = %e, "Failed to handle input");
                         break;
                    }
                }

                // Write to Socket (from Broadcast)
                Ok(tag) = Self::wait_for_tag(&mut self.active_stream_rx) => {
                    if let Err(e) = self.handle_broadcast_tag(tag).await {
                         // Client might have disconnected or socket error
                         warn!(remote_addr = ?addr, error = %e, "Failed to send tag to client");
                         break;
                    }
                }
            }
        }
        Ok(())
    }

    async fn wait_for_tag(
        rx: &mut Option<broadcast::Receiver<Arc<FlvTag>>>,
    ) -> Result<Arc<FlvTag>> {
        match rx {
            Some(rx) => rx.recv().await.map_err(|e| e.into()),
            None => std::future::pending().await,
        }
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
                _ => {} // Ignore unhandleable for now
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
                info!("Connection requested: {}", app_name);

                let res = if self.appname == app_name {
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
                info!("Play requested: {} (ID: {})", stream_key, stream_id);

                let res = if self.ingest_manager.has_stream(&stream_key).await {
                    self.current_stream_id = stream_id;

                    let (rx, state) = self.dispatcher.subscribe(&stream_key).await;
                    self.active_stream_rx = Some(rx);
                    self.stream_state = Some(state);

                    self.session_mut()?.accept_request(request_id)?
                } else {
                    self.session_mut()?.reject_request(
                        request_id,
                        "StreamNotFound",
                        "Stream not found",
                    )?
                };

                self.write_response(res).await?;
            }
            ServerSessionEvent::PlayStreamFinished { .. } => {
                self.active_stream_rx = None;
                self.stream_state = None;
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_broadcast_tag(&mut self, tag: Arc<FlvTag>) -> Result<()> {
        // Send cached headers first if not sent yet
        if !self.sent_headers {
            self.send_cached_headers().await?;
            self.sent_headers = true;
        }

        self.send_tag(tag.as_ref()).await
    }

    async fn send_cached_headers(&mut self) -> Result<()> {
        let state = if let Some(s) = &self.stream_state {
            s.clone()
        } else {
            return Ok(());
        };

        if let Some(tag) = state.video_seq_header.read().await.clone() {
            self.send_tag(&tag).await?;
        }

        if let Some(tag) = state.audio_seq_header.read().await.clone() {
            self.send_tag(&tag).await?;
        }
        Ok(())
    }

    async fn send_tag(&mut self, tag: &FlvTag) -> Result<()> {
        let stream_id = self.current_stream_id;
        let session = self.session_mut()?;

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
        self.socket.write_all(&packet.bytes).await?;
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
