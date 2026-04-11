use anyhow::Result;
use tokio::sync::Mutex;
use tracing::warn;

use crate::abstraction::{MiddlewareTrait, PipeContextTrait};
use crate::infra::media::context::FlvOutputContext;
use crate::infra::media::packet::{FlvTag, UnifiedPacket};
use crate::infra::media::stream::StreamCollection;
use crate::pipeline::UnifiedPacketContext;
use crate::queue::{Channel, ChannelSendStatus, MpscChannel};
use crate::transport::contract::message::StreamFlvTag;

use std::sync::Arc;

struct ForwardState {
    flv_ctx: FlvOutputContext,
}

pub struct FlvMuxForwardMiddleware {
    stream_id: String,
    streams: Arc<dyn StreamCollection + Send + Sync>,
    direct_forward_channel: MpscChannel<StreamFlvTag>,
    state: Mutex<ForwardState>,
}

impl FlvMuxForwardMiddleware {
    pub fn new(
        stream_id: String,
        streams: Arc<dyn StreamCollection + Send + Sync>,
        flv_relay_queue_capacity: usize,
        rtmp_tag_channel: MpscChannel<StreamFlvTag>,
    ) -> Result<Self> {
        let direct_forward_channel = rtmp_tag_channel
            .clone()
            .with_source("pipeline.flv_mux.direct")
            .with_live_id(stream_id.clone());

        let flv_relay_channel = Channel::mpsc_bounded(
            "flv_relay",
            "pipeline.flv_mux.flv_output_tx",
            flv_relay_queue_capacity,
        )
        .with_live_id(stream_id.clone());
        let flv_ctx = FlvOutputContext::create(flv_relay_channel.clone(), streams.as_ref())?;
        Self::spawn_flv_relay(
            flv_relay_channel.with_source("pipeline.flv_mux.relay_channel"),
            rtmp_tag_channel.clone(),
            stream_id.clone(),
        );

        Ok(Self {
            stream_id,
            streams,
            direct_forward_channel,
            state: Mutex::new(ForwardState { flv_ctx }),
        })
    }

    fn spawn_flv_relay(
        relay_channel: MpscChannel<FlvTag>,
        rtmp_tag_channel: MpscChannel<StreamFlvTag>,
        stream_id: String,
    ) {
        tokio::spawn(async move {
            let mut tags = match relay_channel.subscribe("pipeline.flv_mux.relay_rx") {
                Ok(stream) => stream,
                Err(e) => {
                    warn!(error = %e, "Failed to subscribe FLV relay stream");
                    return;
                }
            };
            let relay_tx_channel = rtmp_tag_channel
                .with_source("pipeline.flv_mux.relay_tx")
                .with_live_id(stream_id.clone());

            while let Some(tag) = tags.next().await {
                if matches!(
                    relay_tx_channel.send(StreamFlvTag::new(stream_id.clone(), tag)),
                    ChannelSendStatus::Disconnected
                ) {
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
                packet.rescale_ts_for_stream(self.streams.as_ref(), &state.flv_ctx)?;
                packet.write(&state.flv_ctx)?;
            }
            UnifiedPacket::FlvTag(tag) => {
                if matches!(
                    self.direct_forward_channel
                        .send(StreamFlvTag::new(self.stream_id.clone(), tag.clone())),
                    ChannelSendStatus::Disconnected
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
