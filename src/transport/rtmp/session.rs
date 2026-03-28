use anyhow::Result;
use bytes::BytesMut;
use crossfire::MTx;
use crossfire::mpsc::List;
use rml_rtmp::chunk_io::Packet;
use rml_rtmp::sessions::{ServerSession, ServerSessionEvent, ServerSessionResult};
use rml_rtmp::time::RtmpTimestamp;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::media::format::FlvTag;
use crate::transport::message::StreamEvent;
use crate::transport::{ConnectionState, SessionState, global};

use super::RtmpConnection;
use super::handler::HandlerBuilder;

pub struct SessionGuard {
    connection: RtmpConnection,
    session: ServerSession,
    appname: String,

    event_tx: MTx<List<StreamEvent>>,

    chunk_size: u32,
}

pub(super) struct SessionGuardBuilder {
    connection: RtmpConnection,
    session: Option<ServerSession>,
    appname: Option<String>,
    event_tx: Option<MTx<List<StreamEvent>>>,
}

impl SessionGuard {
    pub(self) fn new(
        connection: RtmpConnection,
        session: ServerSession,
        appname: String,
        event_tx: MTx<List<StreamEvent>>,
    ) -> Self {
        Self {
            connection,
            session,
            appname,
            event_tx,
            chunk_size: 2048,
        }
    }

    pub(super) fn set_chunk_size(&mut self, new_chunk_size: u32) {
        self.chunk_size = new_chunk_size;
    }

    pub(super) async fn read_result(
        &mut self,
        ct: &CancellationToken,
    ) -> Result<Vec<ServerSessionResult>> {
        let mut buffer = BytesMut::with_capacity(self.chunk_size as usize);

        self.connection.read(&mut buffer, ct).await?;
        Ok(self.session.handle_input(&buffer)?)
    }

    async fn handle_packet(&mut self, packet: Packet, ct: &CancellationToken) -> Result<()> {
        self.connection.write(&packet.bytes, ct).await?;
        Ok(())
    }

    pub(super) async fn handle_results(
        &mut self,
        results: Vec<ServerSessionResult>,
        ct: &CancellationToken,
    ) -> Result<Vec<ServerSessionEvent>> {
        let mut events = Vec::new();
        for result in results {
            match result {
                ServerSessionResult::OutboundResponse(packet) => {
                    self.handle_packet(packet, ct).await?;
                }
                ServerSessionResult::RaisedEvent(event) => {
                    events.push(event);
                }
                ServerSessionResult::UnhandleableMessageReceived(payload) => {
                    debug!(payload = ?payload, "Received non-response RTMP session result");
                    anyhow::bail!("Received unhandleable message: {:?}", payload);
                }
            }
        }

        Ok(events)
    }

    pub(super) async fn send_flv_tag(
        &mut self,
        stream_id: u32,
        tag: FlvTag,
        ct: &CancellationToken,
    ) -> Result<()> {
        fn to_timestamp(timestamp: u32) -> RtmpTimestamp {
            RtmpTimestamp::new(timestamp)
        }

        let packet = match tag {
            FlvTag::Audio { timestamp, payload } => {
                self.session
                    .send_audio_data(stream_id, payload, to_timestamp(timestamp), false)?
            }
            FlvTag::Video {
                timestamp,
                payload,
                is_keyframe,
            } => self.session.send_video_data(
                stream_id,
                payload,
                to_timestamp(timestamp),
                is_keyframe,
            )?,
            _ => return Ok(()),
        };

        self.handle_packet(packet, ct).await?;

        Ok(())
    }

    pub(super) async fn finish_playing(
        &mut self,
        stream_id: u32,
        ct: &CancellationToken,
    ) -> Result<()> {
        let packet = self.session.finish_playing(stream_id)?;
        self.handle_packet(packet, ct).await?;
        Ok(())
    }

    pub async fn connect(mut self, ct: &CancellationToken) -> Result<HandlerBuilder> {
        loop {
            let results = self.read_result(ct).await?;

            let events = self.handle_results(results, ct).await?;
            for event in events {
                let handler_builder = match self.handle_connect_event(event, ct).await? {
                    Some(builder) => builder,
                    None => continue,
                };

                let handler_builder = handler_builder
                    .with_appname(self.appname.clone())
                    .with_session(self);
                return Ok(handler_builder);
            }
        }
    }

    async fn accept_request(&mut self, request_id: u32, ct: &CancellationToken) -> Result<()> {
        let results = self.session.accept_request(request_id)?;
        self.handle_results(results, ct).await?;
        Ok(())
    }

    async fn reject_request(
        &mut self,
        request_id: u32,
        code: &str,
        description: &str,
        ct: &CancellationToken,
    ) -> Result<()> {
        let results = self.session.reject_request(request_id, code, description)?;
        self.handle_results(results, ct).await?;
        Ok(())
    }

    async fn handle_connect_event(
        &mut self,
        event: ServerSessionEvent,
        ct: &CancellationToken,
    ) -> Result<Option<HandlerBuilder>> {
        match event {
            ServerSessionEvent::ConnectionRequested {
                request_id,
                app_name,
            } => {
                self.on_connect_requested(request_id, app_name, ct).await?;
                Ok(None)
            }
            ServerSessionEvent::PlayStreamRequested {
                request_id,
                stream_key,
                stream_id,
                ..
            } => {
                self.on_play_requested(request_id, stream_key, stream_id, ct)
                    .await
            }
            ServerSessionEvent::PublishStreamRequested {
                request_id,
                stream_key,
                ..
            } => self.on_publish_requested(request_id, stream_key, ct).await,

            _ => {
                debug!(event = ?event, "Unhandled session event");
                anyhow::bail!("Unhandled session event: {:?}", event);
            }
        }
    }

    async fn on_connect_requested(
        &mut self,
        request_id: u32,
        app_name: String,
        ct: &CancellationToken,
    ) -> Result<()> {
        if app_name != self.appname {
            debug!(app_name = %app_name, expected_app = %self.appname, "Client requested connection to unexpected app");
            self.reject_request(request_id, "AppNotFound", "Application not found", ct)
                .await?;
            anyhow::bail!(
                "Client requested connection to unexpected app: {}",
                app_name
            );
        }

        self.accept_request(request_id, ct).await?;
        Ok(())
    }

    async fn on_play_requested(
        &mut self,
        request_id: u32,
        stream_key: String,
        stream_id: u32,
        ct: &CancellationToken,
    ) -> Result<Option<HandlerBuilder>> {
        let is_active = match global::get_session(&stream_key).await {
            Some(session) => {
                let session_guard = session.read().await;
                ConnectionState::Connected == session_guard.state.into()
            }
            None => false,
        };
        if !is_active {
            debug!(stream_key = %stream_key, "Client requested to play non-existent or inactive stream");
            self.reject_request(request_id, "StreamNotFound", "Stream not found", ct)
                .await?;
            anyhow::bail!(
                "Client requested to play non-existent or inactive stream: {}",
                stream_key
            );
        }

        self.accept_request(request_id, ct).await?;
        Ok(Some(HandlerBuilder::play(stream_key, stream_id)))
    }

    async fn on_publish_requested(
        &mut self,
        request_id: u32,
        stream_key: String,
        ct: &CancellationToken,
    ) -> Result<Option<HandlerBuilder>> {
        let session = global::get_session(&stream_key).await;
        if session.is_none() {
            debug!(stream_key = %stream_key, "Client requested to publish to a stream that does not exist");
            self.reject_request(request_id, "StreamNotFound", "Stream not found", ct)
                .await?;
            anyhow::bail!(
                "Client requested to publish to a stream that does not exist: {}",
                stream_key
            );
        }

        let session = session.unwrap();
        if SessionState::Rtmp(ConnectionState::Precreate) == session.read().await.state {
            let mut session_guard = session.write().await;

            session_guard.state = SessionState::Rtmp(ConnectionState::Connecting);
            self.event_tx.send(StreamEvent::StateChange {
                live_id: stream_key.clone(),
                new_state: session_guard.state,
            });

            let res = self.accept_request(request_id, ct).await;

            session_guard.state = if res.is_ok() {
                SessionState::Rtmp(ConnectionState::Connected)
            } else {
                SessionState::Rtmp(ConnectionState::Disconnected)
            };
            self.event_tx.send(StreamEvent::StateChange {
                live_id: stream_key.clone(),
                new_state: session_guard.state,
            });

            res?;
        } else {
            debug!(stream_key = %stream_key, "Client requested to publish to a stream that is already active");
            session.write().await.state = SessionState::Rtmp(ConnectionState::Disconnected);

            self.reject_request(
                request_id,
                "StreamAlreadyActive",
                "Stream is already active",
                ct,
            )
            .await?;
            anyhow::bail!(
                "Client requested to publish to a stream that is already active: {}",
                stream_key
            );
        }

        Ok(Some(HandlerBuilder::publish(stream_key, session)))
    }
}

impl SessionGuardBuilder {
    pub fn new(connection: RtmpConnection) -> Self {
        Self {
            connection,
            session: None,
            appname: None,
            event_tx: None,
        }
    }

    pub fn with_session(mut self, session: ServerSession) -> Self {
        self.session = Some(session);
        self
    }

    pub fn with_appname(mut self, appname: String) -> Self {
        self.appname = Some(appname);
        self
    }

    pub fn with_event_tx(mut self, event_tx: MTx<List<StreamEvent>>) -> Self {
        self.event_tx = Some(event_tx);
        self
    }

    pub fn build(self) -> Result<SessionGuard> {
        let session = self
            .session
            .ok_or_else(|| anyhow::anyhow!("Session is required to build SessionGuard"))?;
        let appname = self
            .appname
            .ok_or_else(|| anyhow::anyhow!("App name is required to build SessionGuard"))?;
        let event_tx = self.event_tx.ok_or_else(|| {
            anyhow::anyhow!("Event transmitter is required to build SessionGuard")
        })?;

        Ok(SessionGuard::new(
            self.connection,
            session,
            appname,
            event_tx,
        ))
    }
}
