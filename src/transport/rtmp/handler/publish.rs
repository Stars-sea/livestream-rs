use std::sync::Arc;

use anyhow::Result;
use rml_rtmp::sessions::ServerSessionEvent;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::infra::media::packet::FlvTag;
use crate::pipeline::{PipeBus, UnifiedPacketContext};
use crate::transport::lifecycle::HandlerLifecycle;
use crate::transport::rtmp::handler::HandlerTrait;
use crate::transport::rtmp::session::SessionGuard;

pub struct PublishHandler {
    session: SessionGuard,

    #[allow(unused)]
    appname: String,
    stream_key: String,
    bus: PipeBus,

    lifecycle: HandlerLifecycle,
    cancel_token: CancellationToken,
}

impl PublishHandler {
    pub(super) fn new(
        session: SessionGuard,
        appname: String,
        stream_key: String,
        bus: PipeBus,
        lifecycle: HandlerLifecycle,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            session,
            appname,
            stream_key,
            bus,
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

    async fn send_publish_tag(&self, tag: FlvTag, source: &'static str) -> Result<()> {
        if let Err(e) = self.lifecycle.connect().await {
            warn!(stream_key = %self.stream_key, error = %e, "Failed to emit RTMP connected state on publish tag");
        }

        if let FlvTag::ScriptData(meta) = &tag {
            self.lifecycle.init(Arc::new(meta.clone())).await;
        }

        let context = UnifiedPacketContext::new(
            self.stream_key.clone(),
            tag.into(),
            self.cancel_token.clone(),
        );
        self.bus.send_packet(context).await.map_err(|e| {
            anyhow::anyhow!(
                "Failed to forward RTMP tag to pipeline at {}: {}",
                source,
                e
            )
        })
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
                self.send_publish_tag(flv_tag, "rtmp.publish.audio").await?;
            }
            ServerSessionEvent::VideoDataReceived {
                data, timestamp, ..
            } => {
                let flv_tag = FlvTag::video(timestamp.value, data);
                self.send_publish_tag(flv_tag, "rtmp.publish.video").await?;
            }
            ServerSessionEvent::StreamMetadataChanged { metadata, .. } => {
                let flv_tag = FlvTag::script_data(metadata);
                self.send_publish_tag(flv_tag, "rtmp.publish.metadata")
                    .await?;
            }

            _ => {
                debug!(event = ?event, "Received non-publish RTMP session event");
            }
        }

        Ok(())
    }
}
