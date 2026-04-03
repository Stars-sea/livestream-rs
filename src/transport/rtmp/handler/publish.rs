use anyhow::Result;
use crossfire::{MAsyncTx, mpsc::Array};
use rml_rtmp::sessions::ServerSessionEvent;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::infra::media::packet::FlvTag;
use crate::transport::contract::message::StreamEvent;
use crate::transport::contract::state::{RtmpState, SessionState};
use crate::transport::rtmp::handler::HandlerTrait;
use crate::transport::rtmp::session::SessionGuard;
use crate::transport::rtmp::tag::WrappedFlvTag;

pub struct PublishHandler {
    session: SessionGuard,

    #[allow(unused)]
    appname: String,
    stream_key: String,

    tag_tx: MAsyncTx<Array<WrappedFlvTag>>,

    cancel_token: CancellationToken,
}

impl PublishHandler {
    pub(super) fn new(
        session: SessionGuard,
        appname: String,
        stream_key: String,
        tag_tx: MAsyncTx<Array<WrappedFlvTag>>,
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

    fn publish_finished(&mut self) -> Result<()> {
        debug!("Publish finished for stream key: {}", self.stream_key);

        self.session.event_tx.send(StreamEvent::StateChange {
            live_id: self.stream_key.clone(),
            new_state: SessionState::Rtmp(RtmpState::Disconnected),
        })?;
        self.cancel_token.cancel();
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
                self.publish_finished()?;
            }
            ServerSessionEvent::AudioDataReceived {
                data, timestamp, ..
            } => {
                let flv_tag = FlvTag::audio(timestamp.value, data);
                self.tag_tx
                    .send(WrappedFlvTag::new(
                        &self.stream_key,
                        flv_tag,
                        &self.cancel_token,
                    ))
                    .await?;
            }
            ServerSessionEvent::VideoDataReceived {
                data, timestamp, ..
            } => {
                let flv_tag = FlvTag::video(timestamp.value, data);
                self.tag_tx
                    .send(WrappedFlvTag::new(
                        &self.stream_key,
                        flv_tag,
                        &self.cancel_token,
                    ))
                    .await?;
            }
            ServerSessionEvent::StreamMetadataChanged { metadata, .. } => {
                let flv_tag = FlvTag::script_data(metadata);
                self.tag_tx
                    .send(WrappedFlvTag::new(
                        &self.stream_key,
                        flv_tag,
                        &self.cancel_token,
                    ))
                    .await?;
            }

            _ => {
                debug!(event = ?event, "Received non-publish RTMP session event");
            }
        }

        Ok(())
    }
}
