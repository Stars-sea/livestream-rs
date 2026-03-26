use anyhow::Result;
use crossfire::MAsyncRx;
use crossfire::mpmc::List;
use rml_rtmp::sessions::ServerSessionEvent;
use tracing::{debug, info};

use crate::media::flv_parser::FlvTag;
use crate::transport::rtmp::{SessionGuard, handler::HandlerTrait};

pub struct PlayHandler {
    session: SessionGuard,

    appname: String,
    stream_key: String,
    stream_id: u32,

    flv_tag_rx: MAsyncRx<List<FlvTag>>,
}

impl PlayHandler {
    pub(super) fn new(
        session: SessionGuard,
        appname: String,
        stream_key: String,
        stream_id: u32,
        flv_tag_rx: MAsyncRx<List<FlvTag>>,
    ) -> Self {
        Self {
            session,
            appname,
            stream_key,
            stream_id,
            flv_tag_rx,
        }
    }

    async fn send_flv_tag(&mut self, tag: FlvTag) -> Result<()> {
        debug!(
            "Sending FLV tag for stream key {}: {:?}",
            self.stream_key, tag
        );

        self.session.send_flv_tag(self.stream_id, tag).await
    }
}

#[async_trait::async_trait]
impl HandlerTrait for PlayHandler {
    fn session(&mut self) -> &mut SessionGuard {
        &mut self.session
    }

    async fn handle(&mut self) -> Result<()> {
        let rx = self.flv_tag_rx.clone();
        tokio::select! {
            res = HandlerTrait::handle(self) => res,

            tag = rx.recv() => {
                self.send_flv_tag(tag?).await
            }
        }
    }

    async fn on_custom_events(&mut self, event: ServerSessionEvent) -> Result<()> {
        match event {
            ServerSessionEvent::PlayStreamFinished { .. } => {
                info!("Play finished for stream key: {}", self.stream_key);
            }
            ServerSessionEvent::StreamMetadataChanged { .. } => {
                debug!(
                    "Stream metadata changed for stream key: {}",
                    self.stream_key
                );
            }

            _ => {
                debug!(event = ?event, "Received non-play RTMP session event");
            }
        }

        Ok(())
    }
}
