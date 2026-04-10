use anyhow::Result;
use rml_rtmp::sessions::{ServerSessionError, ServerSessionEvent, ServerSessionResult};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::infra::media::packet::FlvTag;
use crate::transport::rtmp::handler::HandlerTrait;
use crate::transport::rtmp::session::SessionGuard;

pub struct PlayHandler {
    session: SessionGuard,

    #[allow(unused)]
    appname: String,
    stream_key: String,
    stream_id: u32,

    tag_rx: broadcast::Receiver<FlvTag>,
    cached_tags: Vec<FlvTag>,
    waiting_keyframe: bool,

    cancel_token: CancellationToken,
}

impl PlayHandler {
    pub(super) fn new(
        session: SessionGuard,
        appname: String,
        stream_key: String,
        stream_id: u32,
        tag_rx: broadcast::Receiver<FlvTag>,
        cached_tags: Vec<FlvTag>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            session,
            appname,
            stream_key,
            stream_id,
            tag_rx,
            cached_tags,
            waiting_keyframe: false,
            cancel_token,
        }
    }

    async fn send_flv_tag(&mut self, tag: FlvTag) -> Result<()> {
        debug!(stream_key = %self.stream_key, payload_size = tag.payload_size(), "Sending FLV tag");

        self.session
            .send_flv_tag(self.stream_id, tag, &self.cancel_token)
            .await
    }

    async fn send_cached_tags(&mut self) -> Result<()> {
        for tag in std::mem::take(&mut self.cached_tags) {
            self.send_flv_tag(tag).await?;
        }
        Ok(())
    }

    async fn handle_session_results(
        &mut self,
        results: Vec<ServerSessionResult>,
        ct: &CancellationToken,
    ) -> Result<()> {
        let events = self.session.handle_results(results, ct).await?;

        for event in events {
            if let Some(event) = self.on_common_events(event).await? {
                self.on_custom_events(event).await?;
            }
        }

        Ok(())
    }

    fn should_skip_while_waiting_keyframe(&mut self, tag: &FlvTag) -> bool {
        if !self.waiting_keyframe {
            return false;
        }

        match tag {
            FlvTag::Video {
                is_keyframe: true, ..
            } => {
                self.waiting_keyframe = false;
                warn!(stream_key = %self.stream_key, "Recovered RTMP play stream at next keyframe after lag");
                false
            }
            FlvTag::Video { .. } => true,
            _ => false,
        }
    }

    async fn handle_tag_recv(&mut self, recv_result: Result<FlvTag, RecvError>) -> Result<()> {
        let tag = match recv_result {
            Ok(tag) => tag,
            Err(RecvError::Lagged(skipped)) => {
                // Skip stale tags and keep connection alive; decoder will recover on subsequent keyframe.
                self.waiting_keyframe = true;
                warn!(stream_key = %self.stream_key, skipped = skipped, "RTMP play receiver lagged, dropping stale tags");
                return Ok(());
            }
            Err(RecvError::Closed) => {
                anyhow::bail!(
                    "RTMP play tag channel closed for stream {}",
                    self.stream_key
                );
            }
        };

        if self.should_skip_while_waiting_keyframe(&tag) {
            return Ok(());
        }

        self.send_flv_tag(tag).await
    }

    async fn finish_playing(&mut self) -> Result<()> {
        fn is_ignorable_finish_error(stream_id: u32, e: &anyhow::Error) -> bool {
            matches!(
                e.downcast_ref::<ServerSessionError>(),
                Some(ServerSessionError::ActionAttemptedOnInactiveStream { action, stream_id: id })
                    if action == "complete" && *id == stream_id
            )
        }

        debug!("Finishing play for stream key: {}", self.stream_key);
        if let Err(e) = self
            .session
            .finish_playing(self.stream_id, &self.cancel_token)
            .await
        {
            if is_ignorable_finish_error(self.stream_id, &e) {
                debug!(stream_key = %self.stream_key, stream_id = self.stream_id, "Ignoring finish_playing on missing stream id");
                return Ok(());
            }
            return Err(e);
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl HandlerTrait for PlayHandler {
    fn session(&mut self) -> &mut SessionGuard {
        &mut self.session
    }

    fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    async fn handle(&mut self) -> Result<()> {
        let ct = self.cancel_token();
        self.send_cached_tags().await?;

        loop {
            tokio::select! {
                _ = ct.cancelled() => return Ok(()),
                results = self.session.read_result(&ct) => {
                    self.handle_session_results(results?, &ct).await?;
                }
                tag = self.tag_rx.recv() => self.handle_tag_recv(tag).await?,

            }
        }
    }

    async fn on_custom_events(&mut self, event: ServerSessionEvent) -> Result<()> {
        match event {
            ServerSessionEvent::PlayStreamFinished { .. } => {
                self.finish_playing().await?;
            }
            _ => {
                debug!(event = ?event, "Received non-play RTMP session event");
            }
        }

        Ok(())
    }
}
