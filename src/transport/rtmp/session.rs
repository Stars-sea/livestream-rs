use anyhow::Result;
use bytes::BytesMut;
use rml_rtmp::sessions::{ServerSession, ServerSessionEvent, ServerSessionResult};
use tracing::debug;

use crate::transport::rtmp::handler::HandlerType;

use super::{Handler, RtmpConnection};

pub struct SessionGuard {
    connection: RtmpConnection,
    session: ServerSession,
    appname: String,

    chunk_size: u32,
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

    pub(super) async fn handle_results(
        &mut self,
        results: Vec<ServerSessionResult>,
    ) -> Result<Vec<ServerSessionEvent>> {
        let mut events = Vec::new();
        for result in results {
            match result {
                ServerSessionResult::OutboundResponse(packet) => {
                    self.connection.write(&packet.bytes).await?;
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

    pub async fn connect(mut self) -> Result<Handler> {
        loop {
            let results = self.read_result().await?;

            let events = self.handle_results(results).await?;
            for event in events {
                if let Some((stream_key, handler_type)) = self.handle_connect_event(event).await? {
                    let appname = self.appname.clone();
                    return Ok(Handler::new(self, appname, stream_key, handler_type));
                }
            }
        }
    }

    async fn handle_connect_event(
        &mut self,
        event: ServerSessionEvent,
    ) -> Result<Option<(String, HandlerType)>> {
        match event {
            ServerSessionEvent::ConnectionRequested {
                request_id,
                app_name,
            } => {
                if app_name != self.appname {
                    debug!(app_name = %app_name, expected_app = %self.appname, "Client requested connection to unexpected app");
                    let results = self.session.reject_request(
                        request_id,
                        "AppNotFound",
                        "Application not found",
                    )?;
                    self.handle_results(results).await?;
                    anyhow::bail!(
                        "Client requested connection to unexpected app: {}",
                        app_name
                    );
                }

                let results = self.session.accept_request(request_id)?;
                self.handle_results(results).await?;
            }
            ServerSessionEvent::PlayStreamRequested {
                request_id,
                stream_key,
                ..
            } => {
                // TODO: Validate stream key format, check if stream exists, etc.

                let results = self.session.accept_request(request_id)?;
                self.handle_results(results).await?;

                return Ok(Some((stream_key, HandlerType::Play)));
            }
            ServerSessionEvent::PublishStreamRequested {
                request_id,
                stream_key,
                ..
            } => {
                // TODO: Validate stream key format, check if stream already exists, etc.

                let results = self.session.accept_request(request_id)?;
                self.handle_results(results).await?;

                return Ok(Some((stream_key, HandlerType::Publish)));
            }
            _ => {
                debug!(event = ?event, "Unhandled session event");
                anyhow::bail!("Unhandled session event: {:?}", event);
            }
        }

        Ok(None)
    }
}

pub(super) struct SessionGuardBuilder {
    connection: RtmpConnection,
    session: Option<ServerSession>,
    appname: Option<String>,
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

        Ok(SessionGuard::new(
            self.connection,
            session,
            appname,
            // stream_key,
        ))
    }
}
