use anyhow::Result;
use rml_rtmp::sessions::ServerSessionEvent;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::debug;

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

    cancel_token: CancellationToken,
}

impl PlayHandler {
    pub(super) fn new(
        session: SessionGuard,
        appname: String,
        stream_key: String,
        stream_id: u32,
        tag_rx: broadcast::Receiver<FlvTag>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            session,
            appname,
            stream_key,
            stream_id,
            tag_rx,
            cancel_token,
        }
    }

    async fn send_flv_tag(&mut self, tag: FlvTag) -> Result<()> {
        debug!(
            "Sending FLV tag for stream key {}: {:?}",
            self.stream_key, tag
        );

        self.session
            .send_flv_tag(self.stream_id, tag, &self.cancel_token)
            .await
    }

    async fn finish_playing(&mut self) -> Result<()> {
        debug!("Finishing play for stream key: {}", self.stream_key);
        self.session
            .finish_playing(self.stream_id, &self.cancel_token)
            .await?;
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

        loop {
            tokio::select! {
                results = self.session.read_result(&ct) => {
                    let results = results?;
                    let events = self.session.handle_results(results, &ct).await?;

                    for event in events {
                        if let Some(event) = self.on_common_events(event).await? {
                            self.on_custom_events(event).await?;
                        }
                    }
                }

                tag = self.tag_rx.recv() => {
                    self.send_flv_tag(tag?).await?;
                }
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
