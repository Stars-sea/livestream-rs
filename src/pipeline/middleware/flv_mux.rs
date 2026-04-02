use anyhow::Result;
use crossfire::mpsc::List;
use crossfire::{AsyncRx, MTx, mpsc};
use tracing::warn;

use crate::abstraction::{MiddlewareTrait, PipeContextTrait};
use crate::dispatcher::{self, SessionEvent};
use crate::infra::ContextSlotMap;
use crate::infra::media::StreamCollection;
use crate::infra::media::context::FlvOutputContext;
use crate::infra::media::packet::{FlvTag, UnifiedPacket};
use crate::pipeline::UnifiedPacketContext;
use crate::transport::contract::message::StreamFlvTag;

use std::sync::Arc;

struct ForwardState {
    flv_ctx: FlvOutputContext,
    streams: Arc<dyn StreamCollection + Send + Sync>,
}

pub struct FlvMuxForwardMiddleware {
    rtmp_tag_tx: MTx<List<StreamFlvTag>>,
    contexts: ContextSlotMap<ForwardState>,
}

impl FlvMuxForwardMiddleware {
    pub fn new(rtmp_tag_tx: MTx<List<StreamFlvTag>>) -> Self {
        let contexts = ContextSlotMap::new();
        Self::spawn_event_listener(contexts.clone(), rtmp_tag_tx.clone());

        Self {
            rtmp_tag_tx,
            contexts,
        }
    }

    fn spawn_event_listener(
        contexts: ContextSlotMap<ForwardState>,
        rtmp_tag_tx: MTx<List<StreamFlvTag>>,
    ) {
        tokio::spawn(async move {
            let dispatcher = dispatcher::singleton().await;
            let mut rx = dispatcher.subscribe();

            while let Ok(event) = rx.recv().await {
                if let Err(e) = Self::session_event_handler(&contexts, &rtmp_tag_tx, event).await {
                    warn!(error = %e, "Error handling session event in FlvMuxForwardMiddleware");
                }
            }
        });
    }

    async fn session_event_handler(
        contexts: &ContextSlotMap<ForwardState>,
        rtmp_tag_tx: &MTx<List<StreamFlvTag>>,
        event: SessionEvent,
    ) -> Result<()> {
        match event {
            SessionEvent::SessionInit { live_id, streams } => {
                let (flv_tx, flv_rx) = mpsc::unbounded_async();
                let flv_ctx = FlvOutputContext::create(flv_tx, streams.as_ref())?;

                Self::spawn_flv_relay(flv_rx, rtmp_tag_tx.clone(), live_id.clone()).await;

                contexts
                    .with_or_create(&live_id, |state| {
                        *state = Some(ForwardState { flv_ctx, streams });
                    })
                    .await;
            }
            SessionEvent::SessionEnded { live_id, .. } => {
                let _ = contexts
                    .with_removed(&live_id, |state| {
                        *state = None;
                    })
                    .await;
            }
            _ => {}
        }

        Ok(())
    }

    async fn spawn_flv_relay(
        rx: AsyncRx<List<FlvTag>>,
        rtmp_tag_tx: MTx<List<StreamFlvTag>>,
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

        match ctx.payload() {
            UnifiedPacket::AVPacket(packet) => {
                self.contexts
                    .with_or_create(&stream_id, |state| -> Result<()> {
                        if let Some(state) = state.as_mut() {
                            let mut packet = packet.clone();
                            packet.rescale_ts_for_stream(state.streams.as_ref(), &state.flv_ctx)?;
                            packet.write(&state.flv_ctx)?;
                        } else {
                            warn!(stream_id = %stream_id, "RTMP forward context not initialized");
                        }
                        Ok(())
                    })
                    .await?;
            }
            UnifiedPacket::FlvTag(tag) => {
                if let Err(e) = self
                    .rtmp_tag_tx
                    .send(StreamFlvTag::new(stream_id.clone(), tag.clone()))
                {
                    anyhow::bail!("Failed to forward FLV tag for stream {}: {}", stream_id, e);
                }
            }
        }

        Ok(ctx)
    }
}
