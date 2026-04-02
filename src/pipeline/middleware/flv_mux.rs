use anyhow::Result;
use crossfire::mpsc::List;
use crossfire::{AsyncRx, MTx, mpsc};
use dashmap::DashMap;
use tokio::sync::Mutex;
use tracing::warn;

use crate::abstraction::{MiddlewareTrait, PipeContextTrait};
use crate::dispatcher::{self, SessionEvent};
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
    contexts: Arc<DashMap<String, Arc<Mutex<Option<ForwardState>>>>>,
}

impl FlvMuxForwardMiddleware {
    pub fn new(rtmp_tag_tx: MTx<List<StreamFlvTag>>) -> Self {
        let contexts = Arc::new(DashMap::new());
        Self::spawn_cleanup_listener(contexts.clone());

        Self {
            rtmp_tag_tx,
            contexts,
        }
    }

    fn get_or_create_ctx_slot(&self, stream_id: &str) -> Arc<Mutex<Option<ForwardState>>> {
        self.contexts
            .entry(stream_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone()
    }

    fn spawn_cleanup_listener(contexts: Arc<DashMap<String, Arc<Mutex<Option<ForwardState>>>>>) {
        tokio::spawn(async move {
            let dispatcher = dispatcher::singleton().await;
            let mut rx = dispatcher.subscribe();

            while let Ok(event) = rx.recv().await {
                if let SessionEvent::SessionEnded { live_id, .. } = event {
                    if let Some((_, slot)) = contexts.remove(&live_id) {
                        let mut guard = slot.lock().await;
                        *guard = None;
                    }
                }
            }
        });
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

    async fn init_stream(
        &self,
        stream_id: &str,
        streams: Arc<dyn StreamCollection + Send + Sync>,
    ) -> Result<()> {
        let (flv_tx, flv_rx) = mpsc::unbounded_async();
        let flv_ctx = FlvOutputContext::create(flv_tx, streams.as_ref())?;

        Self::spawn_flv_relay(flv_rx, self.rtmp_tag_tx.clone(), stream_id.to_string()).await;

        let slot = self.get_or_create_ctx_slot(stream_id);
        let mut guard = slot.lock().await;
        *guard = Some(ForwardState { flv_ctx, streams });

        Ok(())
    }
}

#[async_trait::async_trait]
impl MiddlewareTrait for FlvMuxForwardMiddleware {
    type Context = UnifiedPacketContext;

    async fn send(&self, ctx: Self::Context) -> Result<Self::Context> {
        let stream_id = ctx.id();

        match ctx.payload() {
            UnifiedPacket::Init(streams) => {
                self.init_stream(&stream_id, streams.clone()).await?;
            }
            UnifiedPacket::AVPacket(packet) => {
                let slot = self.get_or_create_ctx_slot(&stream_id);
                let mut guard = slot.lock().await;

                if let Some(state) = guard.as_mut() {
                    let mut packet = packet.clone();
                    packet.rescale_ts_for_stream(state.streams.as_ref(), &state.flv_ctx)?;
                    packet.write(&state.flv_ctx)?;
                } else {
                    warn!(stream_id = %stream_id, "RTMP forward context not initialized");
                }
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
