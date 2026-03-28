mod builder;
pub mod play;
pub mod publish;

use anyhow::Result;
use rml_rtmp::sessions::ServerSessionEvent;
use tokio_util::sync::CancellationToken;

pub use builder::HandlerBuilder;

use super::handler::play::PlayHandler;
use super::handler::publish::PublishHandler;
use crate::transport::rtmp::session::SessionGuard;

pub enum Handler {
    Play(PlayHandler),
    Publish(PublishHandler),
}

#[async_trait::async_trait]
pub trait HandlerTrait {
    fn session(&mut self) -> &mut SessionGuard;

    fn cancel_token(&self) -> CancellationToken;

    async fn handle(&mut self) -> Result<()> {
        let ct = self.cancel_token();
        loop {
            let results = self.session().read_result(&ct).await?;
            let events = self.session().handle_results(results, &ct).await?;

            for event in events {
                if let Some(event) = self.on_common_events(event).await? {
                    self.on_custom_events(event).await?;
                }
            }
        }
    }

    async fn on_custom_events(&mut self, event: ServerSessionEvent) -> Result<()>;

    async fn on_common_events(
        &mut self,
        event: ServerSessionEvent,
    ) -> Result<Option<ServerSessionEvent>> {
        match event {
            ServerSessionEvent::PingResponseReceived { timestamp: _ } => {
                // TODO: Handle ping response if needed

                Ok(None)
            }
            ServerSessionEvent::ClientChunkSizeChanged { new_chunk_size } => {
                self.session().set_chunk_size(new_chunk_size);
                Ok(None)
            }
            _ => Ok(Some(event)),
        }
    }
}

impl Handler {
    pub async fn handle(&mut self) -> Result<()> {
        match self {
            Handler::Play(handler) => handler.handle().await,
            Handler::Publish(handler) => handler.handle().await,
        }
    }
}
