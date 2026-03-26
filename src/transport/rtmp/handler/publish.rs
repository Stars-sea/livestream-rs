use anyhow::Result;
use rml_rtmp::sessions::ServerSessionEvent;
use tracing::{debug, info};

use crate::transport::rtmp::{SessionGuard, handler::HandlerTrait};

pub struct PublishHandler {
    session: SessionGuard,

    appname: String,
    stream_key: String,
}

impl PublishHandler {
    pub(super) fn new(session: SessionGuard, appname: String, stream_key: String) -> Self {
        Self {
            session,
            appname,
            stream_key,
        }
    }
}

#[async_trait::async_trait]
impl HandlerTrait for PublishHandler {
    fn session(&mut self) -> &mut SessionGuard {
        &mut self.session
    }

    async fn on_custom_events(&mut self, event: ServerSessionEvent) -> Result<()> {
        match event {
            ServerSessionEvent::PublishStreamFinished { .. } => {
                info!("Publish finished for stream key: {}", self.stream_key);
            }
            ServerSessionEvent::AudioDataReceived { .. } => {
                debug!("Audio data received for stream key: {}", self.stream_key);
            }
            ServerSessionEvent::VideoDataReceived { .. } => {
                debug!("Video data received for stream key: {}", self.stream_key);
            }
            ServerSessionEvent::StreamMetadataChanged { .. } => {
                debug!(
                    "Stream metadata changed for stream key: {}",
                    self.stream_key
                );
            }

            _ => {
                debug!(event = ?event, "Received non-publish RTMP session event");
            }
        }

        Ok(())
    }
}
