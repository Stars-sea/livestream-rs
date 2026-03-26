use anyhow::Result;
use crossfire::{MAsyncTx, mpmc::List};
use rml_rtmp::sessions::ServerSessionEvent;
use tracing::debug;

use crate::media::flv_parser::FlvTag;
use crate::transport::rtmp::{SessionGuard, handler::HandlerTrait};

pub struct PublishHandler {
    session: SessionGuard,

    appname: String,
    stream_key: String,

    flv_tag_tx: MAsyncTx<List<FlvTag>>,
}

impl PublishHandler {
    pub(super) fn new(
        session: SessionGuard,
        appname: String,
        stream_key: String,
        flv_tag_tx: MAsyncTx<List<FlvTag>>,
    ) -> Self {
        Self {
            session,
            appname,
            stream_key,
            flv_tag_tx,
        }
    }

    async fn publish_finished(&mut self) -> Result<()> {
        debug!("Publish finished for stream key: {}", self.stream_key);

        // TODO: Clean up any resources associated with this stream key, such as removing it from the stream manager
        Ok(())
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
                self.publish_finished().await?;
            }
            ServerSessionEvent::AudioDataReceived {
                data, timestamp, ..
            } => {
                let flv_tag = FlvTag::audio(timestamp.value, data);
                self.flv_tag_tx.send(flv_tag).await?;
            }
            ServerSessionEvent::VideoDataReceived {
                data, timestamp, ..
            } => {
                let flv_tag = FlvTag::video(timestamp.value, data);
                self.flv_tag_tx.send(flv_tag).await?;
            }
            ServerSessionEvent::StreamMetadataChanged { metadata, .. } => {
                let flv_tag = FlvTag::script_data(metadata);
                self.flv_tag_tx.send(flv_tag).await?;
            }

            _ => {
                debug!(event = ?event, "Received non-publish RTMP session event");
            }
        }

        Ok(())
    }
}
