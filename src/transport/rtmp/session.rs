use anyhow::Result;
use rml_rtmp::sessions::ServerSession;

use super::RtmpConnection;

pub struct SessionGuard {
    connection: RtmpConnection,
    session: ServerSession,
    appname: String,
    stream_key: Option<String>,
}

impl SessionGuard {
    pub(self) fn new(connection: RtmpConnection, session: ServerSession, appname: String) -> Self {
        Self {
            connection,
            session,
            appname,
            stream_key: None,
        }
    }

    pub async fn handle_loop(&mut self) -> Result<()> {
        Ok(())
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
