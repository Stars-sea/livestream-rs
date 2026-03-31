use anyhow::Result;

use crate::abstraction::MiddlewareTrait;
use crate::pipeline::UnifiedPacketContext;

pub struct SegmentMiddleware {}

impl SegmentMiddleware {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for SegmentMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl MiddlewareTrait for SegmentMiddleware {
    type Context = UnifiedPacketContext;

    async fn send(&self, ctx: Self::Context) -> Result<Self::Context> {
        Ok(ctx)
    }
}
