use anyhow::Result;
use crossfire::mpsc::Array;
use crossfire::{AsyncRx, MTx, mpsc};
use tokio::sync::Mutex;
use tracing::warn;

use crate::abstraction::{MiddlewareTrait, PipeContextTrait};
use crate::infra::media::context::FlvOutputContext;
use crate::infra::media::packet::{FlvTag, UnifiedPacket};
use crate::infra::media::stream::StreamCollection;
use crate::pipeline::UnifiedPacketContext;
use crate::transport::contract::message::StreamFlvTag;

use std::sync::Arc;

struct ForwardState {
    flv_ctx: FlvOutputContext,
    streams: Arc<dyn StreamCollection + Send + Sync>,
}

pub struct FlvMuxForwardMiddleware {
    stream_id: String,
    rtmp_tag_tx: MTx<Array<StreamFlvTag>>,
    state: Mutex<ForwardState>,
}

impl FlvMuxForwardMiddleware {
    pub fn new(
        stream_id: String,
        streams: Arc<dyn StreamCollection + Send + Sync>,
        flv_relay_queue_capacity: usize,
        rtmp_tag_tx: MTx<Array<StreamFlvTag>>,
    ) -> Result<Self> {
        let (flv_tx, flv_rx) = mpsc::bounded_blocking_async(flv_relay_queue_capacity);
        let flv_ctx = FlvOutputContext::create(flv_tx, streams.as_ref())?;
        Self::spawn_flv_relay(flv_rx, rtmp_tag_tx.clone(), stream_id.clone());

        Ok(Self {
            stream_id,
            rtmp_tag_tx,
            state: Mutex::new(ForwardState { flv_ctx, streams }),
        })
    }

    fn spawn_flv_relay(
        rx: AsyncRx<Array<FlvTag>>,
        rtmp_tag_tx: MTx<Array<StreamFlvTag>>,
        stream_id: String,
    ) {
        tokio::spawn(async move {
            while let Ok(tag) = rx.recv().await {
                if rtmp_tag_tx
                    .send(StreamFlvTag::new(stream_id.clone(), tag))
                    .is_err()
                {
                    break;
                }
            }
        });
    }
}

#[async_trait::async_trait]
impl MiddlewareTrait for FlvMuxForwardMiddleware {
    type Context = UnifiedPacketContext;

    async fn send(&self, ctx: Self::Context) -> Result<Self::Context> {
        let stream_id = ctx.id();
        if stream_id != self.stream_id {
            warn!(expected = %self.stream_id, got = %stream_id, "Stream id mismatch in FlvMuxForwardMiddleware");
            return Ok(ctx);
        }

        match ctx.payload() {
            UnifiedPacket::AVPacket(packet) => {
                let mut packet = packet.clone();
                let state = self.state.lock().await;
                packet.rescale_ts_for_stream(state.streams.as_ref(), &state.flv_ctx)?;
                packet.write(&state.flv_ctx)?;
            }
            UnifiedPacket::FlvTag(tag) => {
                if let Err(e) = self
                    .rtmp_tag_tx
                    .send(StreamFlvTag::new(self.stream_id.clone(), tag.clone()))
                {
                    anyhow::bail!("Failed to forward FLV tag for stream {}: {}", stream_id, e);
                }
            }
        }

        Ok(ctx)
    }
}
