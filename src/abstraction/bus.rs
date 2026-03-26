use std::sync::Arc;

use anyhow::Result;

use super::{PipeContextTrait, PipeTrait};

#[async_trait::async_trait]
pub trait BusTrait: Send + Sync {
    async fn send<P, C>(&self, pipe: Arc<P>, context: &mut C) -> Result<()>
    where
        P: PipeTrait<Context = C> + Send + Sync,
        C: PipeContextTrait;
}
