use anyhow::Result;

use super::PipeContextTrait;

#[async_trait::async_trait]
pub trait PipeTrait: Send + Sync {
    type Context: PipeContextTrait;

    async fn send(&self, context: Self::Context) -> Result<Self::Context>;

    async fn close(&self) -> Result<()>;
}
