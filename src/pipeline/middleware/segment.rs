use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use dashmap::DashMap;
use tempfile::{Builder, TempDir};
use tokio::sync::Mutex;

use crate::abstraction::{MiddlewareTrait, PipeContextTrait};
use crate::dispatcher::{self, SessionEvent};
use crate::infra::media::StreamCollection;
use crate::infra::media::context::HlsOutputContext;
use crate::infra::media::packet::{Packet, UnifiedPacket};
use crate::pipeline::UnifiedPacketContext;

struct SegmentState {
    hls_ctx: Option<HlsOutputContext>,
    temp_dir: TempDir,
    streams: Arc<dyn StreamCollection + Send + Sync>,
    segment_id: u64,
    segment_started_at: Instant,
}

impl SegmentState {
    fn new(
        temp_dir: TempDir,
        hls_ctx: HlsOutputContext,
        streams: Arc<dyn StreamCollection + Send + Sync>,
    ) -> Self {
        Self {
            hls_ctx: Some(hls_ctx),
            temp_dir,
            streams,
            segment_id: 0,
            segment_started_at: Instant::now(),
        }
    }

    fn should_rollover(&self, packet: &Packet, segment_duration: Duration) -> bool {
        packet.is_key_frame() && self.segment_started_at.elapsed() >= segment_duration
    }

    fn rollover(&mut self) -> Result<()> {
        self.segment_id = self.segment_id.saturating_add(1);
        let next_ctx = HlsOutputContext::create_segment(
            self.temp_dir.path(),
            self.streams.as_ref(),
            self.segment_id,
        )?;
        self.hls_ctx = Some(next_ctx);
        self.segment_started_at = Instant::now();
        Ok(())
    }
}

impl Drop for SegmentState {
    fn drop(&mut self) {
        // Drop output context first so file handles are closed before TempDir cleanup.
        self.hls_ctx.take();

        // Touch temp_dir to make intent explicit and silence unused-field warnings.
        let _ = self.temp_dir.path();
    }
}

pub struct SegmentMiddleware {
    contexts: Arc<DashMap<String, Arc<Mutex<Option<SegmentState>>>>>,
    segment_duration: Duration,
}

impl SegmentMiddleware {
    pub fn new(segment_duration: Duration) -> Self {
        let contexts = Arc::new(DashMap::new());
        Self::spawn_cleanup_listener(contexts.clone());

        Self {
            contexts,
            segment_duration,
        }
    }

    fn spawn_cleanup_listener(contexts: Arc<DashMap<String, Arc<Mutex<Option<SegmentState>>>>>) {
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

    fn get_or_create_ctx_slot(&self, stream_id: &str) -> Arc<Mutex<Option<SegmentState>>> {
        self.contexts
            .entry(stream_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone()
    }

    fn create_stream_temp_dir() -> Result<TempDir> {
        Ok(Builder::new().prefix("livestream-rs-segment-").tempdir()?)
    }

    async fn write_packet_for_stream(&self, stream_id: &str, packet: Packet) -> Result<()> {
        let slot = self.get_or_create_ctx_slot(stream_id);
        let mut guard = slot.lock().await;

        if let Some(state) = guard.as_mut() {
            if state.should_rollover(&packet, self.segment_duration) {
                state.rollover()?;
            }

            if let Some(hls_ctx) = state.hls_ctx.as_mut() {
                packet.write(hls_ctx)?;
            }
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl MiddlewareTrait for SegmentMiddleware {
    type Context = UnifiedPacketContext;

    async fn send(&self, ctx: Self::Context) -> Result<Self::Context> {
        let stream_id = ctx.id();

        match ctx.payload() {
            UnifiedPacket::Init(streams) => {
                let temp_dir = Self::create_stream_temp_dir()?;
                let hls_ctx =
                    HlsOutputContext::create_segment(temp_dir.path(), streams.as_ref(), 0)?;

                let slot = self.get_or_create_ctx_slot(&stream_id);
                let mut guard = slot.lock().await;
                *guard = Some(SegmentState::new(temp_dir, hls_ctx, streams.clone()));
            }
            UnifiedPacket::AVPacket(packet) => {
                self.write_packet_for_stream(&stream_id, packet.clone())
                    .await?;
            }
            UnifiedPacket::FlvTag(tag) => {
                let maybe_packet: Option<Packet> = tag.clone().into();

                if let Some(packet) = maybe_packet {
                    self.write_packet_for_stream(&stream_id, packet).await?;
                }
            }
        }

        Ok(ctx)
    }
}
