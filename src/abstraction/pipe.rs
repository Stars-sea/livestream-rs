use anyhow::Result;

use crate::abstraction::PipeContextTrait;

#[async_trait::async_trait]
pub trait PipeTrait {
    type Context: PipeContextTrait;

    async fn send(&self, context: &mut Self::Context) -> Result<()>;
}
