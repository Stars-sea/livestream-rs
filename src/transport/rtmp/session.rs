use anyhow::Result;
use bytes::BytesMut;
use rml_rtmp::chunk_io::Packet;
use rml_rtmp::sessions::{ServerSession, ServerSessionEvent, ServerSessionResult};
use rml_rtmp::time::RtmpTimestamp;
use tracing::debug;

use crate::media::format::FlvTag;
use crate::transport::{ConnectionState, SessionState, global_registry};

use super::RtmpConnection;
use super::handler::HandlerBuilder;

pub struct SessionGuard {
    connection: RtmpConnection,
    session: ServerSession,
    appname: String,

    chunk_size: u32,
}

pub(super) struct SessionGuardBuilder {
    connection: RtmpConnection,
    session: Option<ServerSession>,
    appname: Option<String>,
}

impl SessionGuard {
    pub(self) fn new(connection: RtmpConnection, session: ServerSession, appname: String) -> Self {
        Self {
            connection,
            session,
            appname,
            chunk_size: 2048,
        }
    }

    pub(super) fn set_chunk_size(&mut self, new_chunk_size: u32) {
        self.chunk_size = new_chunk_size;
    }

    pub(super) async fn read_result(&mut self) -> Result<Vec<ServerSessionResult>> {
        let mut buffer = BytesMut::with_capacity(self.chunk_size as usize);

        self.connection.read(&mut buffer).await?;
        Ok(self.session.handle_input(&buffer)?)
    }

    async fn handle_packet(&mut self, packet: Packet) -> Result<()> {
        self.connection.write(&packet.bytes).await?;
        Ok(())
    }

    pub(super) async fn handle_results(
        &mut self,
        results: Vec<ServerSessionResult>,
    ) -> Result<Vec<ServerSessionEvent>> {
        let mut events = Vec::new();
        for result in results {
            match result {
                ServerSessionResult::OutboundResponse(packet) => {
                    self.handle_packet(packet).await?;
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

    pub(super) async fn send_flv_tag(&mut self, stream_id: u32, tag: FlvTag) -> Result<()> {
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

        self.handle_packet(packet).await?;

        Ok(())
    }

    pub(super) async fn finish_playing(&mut self, stream_id: u32) -> Result<()> {
        let packet = self.session.finish_playing(stream_id)?;
        self.handle_packet(packet).await?;
        Ok(())
    }

    pub async fn connect(mut self) -> Result<HandlerBuilder> {
        loop {
            let results = self.read_result().await?;

            let events = self.handle_results(results).await?;
            for event in events {
                let handler_builder = match self.handle_connect_event(event).await? {
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

    async fn accept_request(&mut self, request_id: u32) -> Result<()> {
        let results = self.session.accept_request(request_id)?;
        self.handle_results(results).await?;
        Ok(())
    }

    async fn reject_request(
        &mut self,
        request_id: u32,
        code: &str,
        description: &str,
    ) -> Result<()> {
        let results = self.session.reject_request(request_id, code, description)?;
        self.handle_results(results).await?;
        Ok(())
    }

    async fn handle_connect_event(
        &mut self,
        event: ServerSessionEvent,
    ) -> Result<Option<HandlerBuilder>> {
        match event {
            ServerSessionEvent::ConnectionRequested {
                request_id,
                app_name,
            } => {
                if app_name != self.appname {
                    debug!(app_name = %app_name, expected_app = %self.appname, "Client requested connection to unexpected app");
                    self.reject_request(request_id, "AppNotFound", "Application not found")
                        .await?;
                    anyhow::bail!(
                        "Client requested connection to unexpected app: {}",
                        app_name
                    );
                }

                self.accept_request(request_id).await?;
                Ok(None)
            }
            ServerSessionEvent::PlayStreamRequested {
                request_id,
                stream_key,
                stream_id,
                ..
            } => {
                if !global_registry().await.is_active(&stream_key).await {
                    debug!(stream_key = %stream_key, "Client requested to play non-existent or inactive stream");
                    self.reject_request(request_id, "StreamNotFound", "Stream not found")
                        .await?;
                    anyhow::bail!(
                        "Client requested to play non-existent or inactive stream: {}",
                        stream_key
                    );
                }

                self.accept_request(request_id).await?;
                Ok(Some(HandlerBuilder::play(stream_key, stream_id)))
            }
            ServerSessionEvent::PublishStreamRequested {
                request_id,
                stream_key,
                ..
            } => {
                let session = global_registry().await.get(&stream_key);
                if session.is_none() {
                    debug!(stream_key = %stream_key, "Client requested to publish to a stream that does not exist");
                    self.reject_request(request_id, "StreamNotFound", "Stream not found")
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

                    let res = self.accept_request(request_id).await;
                    session_guard.state = if res.is_ok() {
                        SessionState::Rtmp(ConnectionState::Connected)
                    } else {
                        SessionState::Rtmp(ConnectionState::Disconnected)
                    };
                    res?;
                } else {
                    debug!(stream_key = %stream_key, "Client requested to publish to a stream that is already active");
                    session.write().await.state = SessionState::Rtmp(ConnectionState::Disconnected);

                    self.reject_request(
                        request_id,
                        "StreamAlreadyActive",
                        "Stream is already active",
                    )
                    .await?;
                    anyhow::bail!(
                        "Client requested to publish to a stream that is already active: {}",
                        stream_key
                    );
                }

                Ok(Some(HandlerBuilder::publish(stream_key, session)))
            }
            _ => {
                debug!(event = ?event, "Unhandled session event");
                anyhow::bail!("Unhandled session event: {:?}", event);
            }
        }
    }
}

impl SessionGuardBuilder {
    pub fn new(connection: RtmpConnection) -> Self {
        Self {
            connection,
            session: None,
            appname: None,
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

    pub fn build(self) -> Result<SessionGuard> {
        let session = self
            .session
            .ok_or_else(|| anyhow::anyhow!("Session is required to build SessionGuard"))?;
        let appname = self
            .appname
            .ok_or_else(|| anyhow::anyhow!("App name is required to build SessionGuard"))?;

        Ok(SessionGuard::new(self.connection, session, appname))
    }
}
