use anyhow::Result;

use super::PipeContextTrait;

#[async_trait::async_trait]
pub trait MiddlewareTrait: Send + Sync {
    type Context: PipeContextTrait;

    async fn send(&self, context: Self::Context) -> Result<Option<Self::Context>>;
}
