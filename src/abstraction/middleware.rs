use anyhow::Result;

use super::PipeContextTrait;

#[async_trait::async_trait]
pub trait MiddlewareTrait {
    type Context: PipeContextTrait;

    async fn send(&self, context: &mut Self::Context) -> Result<()>;
}
