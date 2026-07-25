use anyhow::Result;
use rml_rtmp::sessions::ServerSessionEvent;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::lifecycle::HandlerLifecycle;
use crate::rtmp::handler::HandlerTrait;
use crate::rtmp::session::SessionGuard;
use crate::source::rtmp::RtmpRawFrame;

pub struct PublishHandler {
    session: SessionGuard,

    stream_key: String,
    source_tx: tokio::sync::mpsc::Sender<RtmpRawFrame>,

    lifecycle: HandlerLifecycle,
    cancel_token: CancellationToken,
}

impl PublishHandler {
    pub(super) fn new(
        session: SessionGuard,
        stream_key: String,
        source_tx: tokio::sync::mpsc::Sender<RtmpRawFrame>,
        lifecycle: HandlerLifecycle,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            session,
            stream_key,
            source_tx,
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

    async fn send_to_source(&self, frame: RtmpRawFrame) -> Result<()> {
        // Drop the frame if lifecycle connect fails.
        // connect() is idempotent — only the first call dispatches SessionStarted.
        if let Err(e) = self.lifecycle.connect().await {
            warn!(stream_key = %self.stream_key, error = %e, "Failed to emit RTMP connected state on publish tag");
            return Err(anyhow::anyhow!(
                "Cannot send to source: lifecycle connect failed: {}",
                e
            ));
        }

        // Send the raw frame to the source for pipeline processing.
        self.source_tx.send(frame).await.map_err(|_| {
            anyhow::anyhow!("RtmpSource receiver closed for stream {}", self.stream_key)
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
                let frame = RtmpRawFrame {
                    data,
                    timestamp: timestamp.value,
                    is_video: false,
                    is_audio: true,
                    is_script_data: false,
                };
                self.send_to_source(frame).await?;
            }
            ServerSessionEvent::VideoDataReceived {
                data, timestamp, ..
            } => {
                let frame = RtmpRawFrame {
                    data,
                    timestamp: timestamp.value,
                    is_video: true,
                    is_audio: false,
                    is_script_data: false,
                };
                self.send_to_source(frame).await?;
            }
            ServerSessionEvent::StreamMetadataChanged { .. } => {
                // Metadata is informational; pipeline processors derive codec
                // info from sequence headers in-band.
            }

            _ => {
                debug!(event = ?event, "Received non-publish RTMP session event");
            }
        }

        Ok(())
    }
}
