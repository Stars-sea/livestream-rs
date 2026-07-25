use std::sync::Arc;

use anyhow::Result;
use rml_rtmp::sessions::ServerSessionEvent;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::lifecycle::HandlerLifecycle;
use crate::rtmp::handler::HandlerTrait;
use crate::rtmp::session::SessionGuard;
use livestream_media::flv::FlvTag;
use livestream_pipeline::broadcast::FlvBroadcast;

pub struct PublishHandler {
    session: SessionGuard,

    stream_key: String,
    flv_broadcast: Arc<dyn FlvBroadcast>,

    lifecycle: HandlerLifecycle,
    cancel_token: CancellationToken,
}

impl PublishHandler {
    pub(super) fn new(
        session: SessionGuard,
        stream_key: String,
        flv_broadcast: Arc<dyn FlvBroadcast>,
        lifecycle: HandlerLifecycle,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            session,
            stream_key,
            flv_broadcast,
            lifecycle,
            cancel_token,
        }
    }

    async fn publish_finished(&mut self) -> Result<()> {
        debug!("Publish finished for stream key: {}", self.stream_key);

        self.lifecycle.disconnect();

        self.cancel_token.cancel();
        Ok(())
    }

    async fn send_publish_tag(&self, tag: FlvTag) -> Result<()> {
        if let Err(e) = self.lifecycle.connect().await {
            warn!(stream_key = %self.stream_key, error = %e, "Failed to emit RTMP connected state on publish tag");
            return Err(anyhow::anyhow!(
                "Cannot broadcast: lifecycle connect failed: {}",
                e
            ));
        }

        // NOTE: lifecycle.init() is skipped in Phase 6 because:
        // 1. StreamMetadata does not implement the new StreamCollection trait
        // 2. The pipeline engine is a stub (PipeBus has been removed)
        // Full pipeline integration will be added in a future phase.

        self.flv_broadcast
            .broadcast(&self.stream_key, tag)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to broadcast RTMP tag: {}", e))
    }
}

#[async_trait::async_trait]
impl HandlerTrait for PublishHandler {
    fn session(&mut self) -> &mut SessionGuard {
        &mut self.session
    }

    fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    async fn on_custom_events(&mut self, event: ServerSessionEvent) -> Result<()> {
        match event {
            ServerSessionEvent::PublishStreamFinished { .. } => {
                self.publish_finished().await?;
            }
            ServerSessionEvent::AudioDataReceived {
                data, timestamp, ..
            } => {
                let flv_tag = FlvTag::audio(timestamp.value, data);
                self.send_publish_tag(flv_tag).await?;
            }
            ServerSessionEvent::VideoDataReceived {
                data, timestamp, ..
            } => {
                let flv_tag = FlvTag::video(timestamp.value, data);
                self.send_publish_tag(flv_tag).await?;
            }
            ServerSessionEvent::StreamMetadataChanged { metadata, .. } => {
                let flv_tag = FlvTag::script_data(metadata);
                self.send_publish_tag(flv_tag).await?;
            }

            _ => {
                debug!(event = ?event, "Received non-publish RTMP session event");
            }
        }

        Ok(())
    }
}
