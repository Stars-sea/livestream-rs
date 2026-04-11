use anyhow::Result;
use rml_rtmp::sessions::ServerSessionEvent;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::infra::media::packet::FlvTag;
use crate::queue::{ChannelSendStatus, MpscChannel};
use crate::transport::contract::message::send_stream_state_change;
use crate::transport::contract::state::{RtmpState, SessionState};
use crate::transport::rtmp::handler::HandlerTrait;
use crate::transport::rtmp::session::SessionGuard;
use crate::transport::rtmp::tag::WrappedFlvTag;

pub struct PublishHandler {
    session: SessionGuard,

    #[allow(unused)]
    appname: String,
    stream_key: String,

    tag_channel: MpscChannel<WrappedFlvTag>,

    cancel_token: CancellationToken,
}

impl PublishHandler {
    pub(super) fn new(
        session: SessionGuard,
        appname: String,
        stream_key: String,
        tag_channel: MpscChannel<WrappedFlvTag>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            session,
            appname,
            stream_key,
            tag_channel,
            cancel_token,
        }
    }

    fn publish_finished(&mut self) -> Result<()> {
        debug!("Publish finished for stream key: {}", self.stream_key);

        send_stream_state_change(
            &self.session.event_channel,
            self.stream_key.clone(),
            SessionState::Rtmp(RtmpState::Disconnected),
            "rtmp.publish.publish_finished",
        )?;
        self.cancel_token.cancel();
        Ok(())
    }

    fn send_publish_tag(&self, tag: FlvTag, source: &'static str) -> Result<()> {
        let channel = self
            .tag_channel
            .clone()
            .with_source(source)
            .with_live_id(self.stream_key.clone());

        match channel.send(WrappedFlvTag::new(
            &self.stream_key,
            tag,
            &self.cancel_token,
        )) {
            ChannelSendStatus::Sent | ChannelSendStatus::Full => Ok(()),
            ChannelSendStatus::Disconnected => {
                anyhow::bail!("RTMP publish queue disconnected")
            }
        }
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
                self.send_publish_tag(flv_tag, "rtmp.publish.audio")?;
            }
            ServerSessionEvent::VideoDataReceived {
                data, timestamp, ..
            } => {
                let flv_tag = FlvTag::video(timestamp.value, data);
                self.send_publish_tag(flv_tag, "rtmp.publish.video")?;
            }
            ServerSessionEvent::StreamMetadataChanged { metadata, .. } => {
                let flv_tag = FlvTag::script_data(metadata);
                self.send_publish_tag(flv_tag, "rtmp.publish.metadata")?;
            }

            _ => {
                debug!(event = ?event, "Received non-publish RTMP session event");
            }
        }

        Ok(())
    }
}
