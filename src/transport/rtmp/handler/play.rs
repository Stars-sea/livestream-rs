use anyhow::Result;
use rml_rtmp::sessions::ServerSessionEvent;
use tracing::{debug, info};

use crate::transport::rtmp::{SessionGuard, handler::HandlerTrait};

pub struct PlayHandler {
    session: SessionGuard,

    appname: String,
    stream_key: String,
}

impl PlayHandler {
    pub fn new(session: SessionGuard, appname: String, stream_key: String) -> Self {
        Self {
            session,
            appname,
            stream_key,
        }
    }
}

#[async_trait::async_trait]
impl HandlerTrait for PlayHandler {
    fn session(&mut self) -> &mut SessionGuard {
        &mut self.session
    }

    async fn on_custom_events(&mut self, event: ServerSessionEvent) -> Result<()> {
        match event {
            ServerSessionEvent::PlayStreamFinished { .. } => {
                info!("Play finished for stream key: {}", self.stream_key);
            }
            ServerSessionEvent::StreamMetadataChanged { .. } => {
                debug!(
                    "Stream metadata changed for stream key: {}",
                    self.stream_key
                );
            }

            _ => {
                debug!(event = ?event, "Received non-play RTMP session event");
            }
        }

        Ok(())
    }
}
