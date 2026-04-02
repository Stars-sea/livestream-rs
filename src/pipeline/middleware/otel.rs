use std::time::Instant;

use anyhow::Result;
use tracing::warn;

use crate::abstraction::{MiddlewareTrait, PipeContextTrait};
use crate::infra::media::packet::UnifiedPacket;
use crate::pipeline::UnifiedPacketContext;
use crate::telemetry::metrics;

pub struct OTelMiddleware {
    stream_id: String,
}

impl OTelMiddleware {
    pub fn new(stream_id: String) -> Self {
        Self { stream_id }
    }
}

#[async_trait::async_trait]
impl MiddlewareTrait for OTelMiddleware {
    type Context = UnifiedPacketContext;

    async fn send(&self, ctx: Self::Context) -> Result<Self::Context> {
        let stream_id = ctx.id();
        if stream_id != self.stream_id {
            warn!(expected = %self.stream_id, got = %stream_id, "Stream id mismatch in OTelPipelineMiddleware");
            return Ok(ctx);
        }

        let started_at = Instant::now();
        let metrics = metrics::get_metrics();

        let bytes = ctx.payload().size() as u64;
        match ctx.payload() {
            UnifiedPacket::AVPacket(_) => {
                metrics.record_pipeline_packet("avpacket", bytes);
            }
            UnifiedPacket::FlvTag(_) => {
                metrics.record_pipeline_packet("flv_tag", bytes);
            }
        }

        metrics.record_middleware_latency("otel_pipeline", started_at.elapsed().as_micros() as u64);
        Ok(ctx)
    }
}
