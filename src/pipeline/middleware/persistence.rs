use anyhow::Result;

use crate::abstraction::MiddlewareTrait;
use crate::pipeline::UnifiedPacketContext;

pub struct PersistenceMiddleware {}

#[async_trait::async_trait]
impl MiddlewareTrait for PersistenceMiddleware {
    type Context = UnifiedPacketContext;

    async fn send(&self, ctx: Self::Context) -> Result<Self::Context> {
        Ok(ctx)
    }
}
