use anyhow::Result;
use tokio::sync::Mutex;
use tracing::warn;

use crate::abstraction::{MiddlewareTrait, PipeContextTrait};
use crate::channel::{self, MpscRx, MpscTx, SendError};
use crate::infra::media::context::FlvOutputContext;
use crate::infra::media::packet::{FlvTag, UnifiedPacket};
use crate::infra::media::stream::StreamCollection;
use crate::pipeline::UnifiedPacketContext;
use crate::transport::abstraction::IngestPacket;

use std::sync::Arc;

struct ForwardState {
    flv_ctx: FlvOutputContext,
}

pub struct FlvMuxForwardMiddleware {
    stream_id: String,
    streams: Arc<dyn StreamCollection + Send + Sync>,
    direct_forward_channel: MpscTx<IngestPacket<FlvTag>>,
    state: Mutex<ForwardState>,
}

impl FlvMuxForwardMiddleware {
    pub fn new(
        stream_id: String,
        streams: Arc<dyn StreamCollection + Send + Sync>,
        flv_relay_queue_capacity: usize,
        rtmp_tag_tx: &MpscTx<IngestPacket<FlvTag>>,
    ) -> Result<Self> {
        let direct_forward_channel = rtmp_tag_tx.clone().with_live_id(stream_id.clone());

        let (tx, rx) = channel::mpsc(
            "flv_relay",
            Some(stream_id.clone()),
            flv_relay_queue_capacity,
        );
        let flv_ctx = FlvOutputContext::create(tx, streams.as_ref())?;
        Self::spawn_flv_relay(rx, direct_forward_channel.clone(), stream_id.clone());

        Ok(Self {
            stream_id,
            streams,
            direct_forward_channel,
            state: Mutex::new(ForwardState { flv_ctx }),
        })
    }

    fn spawn_flv_relay(
        mut tags: MpscRx<FlvTag>,
        rtmp_tag_channel: MpscTx<IngestPacket<FlvTag>>,
        stream_id: String,
    ) {
        tokio::spawn(async move {
            while let Some(tag) = tags.next().await {
                let wrapped = IngestPacket::new(&stream_id, tag);
                if matches!(rtmp_tag_channel.send(wrapped), Err(SendError::Closed)) {
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

        match ctx.payload().to_owned() {
            UnifiedPacket::AVPacket(packet) => {
                let mut packet = packet.clone();
                let state = self.state.lock().await;
                packet.rescale_ts_for_stream(self.streams.as_ref(), &state.flv_ctx)?;
                packet.write(&state.flv_ctx)?;
            }
            UnifiedPacket::FlvTag(tag) => {
                let wrapped = IngestPacket::new(stream_id, tag);
                if matches!(
                    self.direct_forward_channel.send(wrapped),
                    Err(SendError::Closed)
                ) {
                    anyhow::bail!(
                        "Failed to forward FLV tag for stream {}: queue disconnected",
                        stream_id
                    );
                }
            }
        }

        Ok(ctx)
    }
}
