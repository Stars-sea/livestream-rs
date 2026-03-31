use std::sync::Arc;

use anyhow::Result;
use crossfire::{MAsyncTx, mpsc::List};
use rml_rtmp::sessions::ServerSessionEvent;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::infra::media::packet::FlvTag;
use crate::transport::rtmp::handler::HandlerTrait;
use crate::transport::rtmp::session::SessionGuard;
use crate::transport::rtmp::tag::WrappedFlvTag;

pub struct PublishHandler {
    session: SessionGuard,

    #[allow(unused)]
    appname: String,
    stream_key: String,

    tag_tx: MAsyncTx<List<WrappedFlvTag>>,

    cancel_token: CancellationToken,
}

impl PublishHandler {
    pub(super) fn new(
        session: SessionGuard,
        appname: String,
        stream_key: String,
        tag_tx: MAsyncTx<List<WrappedFlvTag>>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            session,
            appname,
            stream_key,
            tag_tx,
            cancel_token,
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
                self.tag_tx
                    .send(WrappedFlvTag::new(&self.stream_key, flv_tag))
                    .await?;
            }
            ServerSessionEvent::VideoDataReceived {
                data, timestamp, ..
            } => {
                let flv_tag = FlvTag::video(timestamp.value, data);
                self.tag_tx
                    .send(WrappedFlvTag::new(&self.stream_key, flv_tag))
                    .await?;
            }
            ServerSessionEvent::StreamMetadataChanged { metadata, .. } => {
                let flv_tag = FlvTag::script_data(Arc::new(metadata));
                self.tag_tx
                    .send(WrappedFlvTag::new(&self.stream_key, flv_tag))
                    .await?;
            }

            _ => {
                debug!(event = ?event, "Received non-publish RTMP session event");
            }
        }

        Ok(())
    }
}
