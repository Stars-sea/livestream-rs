use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tempfile::{Builder, TempDir};
use tokio::sync::Mutex;
use tracing::warn;

use crate::abstraction::{MiddlewareTrait, PipeContextTrait};
use crate::dispatcher::{self, SessionEvent};
use crate::infra::media::context::HlsOutputContext;
use crate::infra::media::packet::{Packet, UnifiedPacket};
use crate::infra::media::stream::StreamCollection;
use crate::pipeline::UnifiedPacketContext;
use crate::pipeline::normalize::normalize_component;
use crate::telemetry::metrics;

struct SegmentState {
    hls_ctx: Option<HlsOutputContext>,
    temp_dir: TempDir,
    segment_id: u64,
    segment_started_at: Instant,
}

impl SegmentState {
    fn new(temp_dir: TempDir, hls_ctx: HlsOutputContext) -> Self {
        Self {
            hls_ctx: Some(hls_ctx),
            temp_dir,
            segment_id: 0,
            segment_started_at: Instant::now(),
        }
    }

    fn should_rollover(&self, packet: &Packet, segment_duration: Duration) -> bool {
        packet.is_key_frame() && self.segment_started_at.elapsed() >= segment_duration
    }

    fn rollover(&mut self, streams: &dyn StreamCollection) -> Result<PathBuf> {
        let completed_segment_path = self
            .hls_ctx
            .as_ref()
            .map(|ctx| ctx.path().clone())
            .ok_or_else(|| anyhow::anyhow!("Missing active HLS context during rollover"))?;

        self.segment_id = self.segment_id.saturating_add(1);
        let next_ctx = HlsOutputContext::create_segment(self.temp_dir.path(), streams, self.segment_id)?;
        self.hls_ctx = Some(next_ctx);
        self.segment_started_at = Instant::now();
        Ok(completed_segment_path)
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
    stream_id: String,
    streams: Arc<dyn StreamCollection + Send + Sync>,
    state: Mutex<SegmentState>,
    segment_duration: Duration,
}

impl SegmentMiddleware {
    pub fn new(
        stream_id: String,
        streams: Arc<dyn StreamCollection + Send + Sync>,
        segment_duration: Duration,
    ) -> Result<Self> {
        let temp_dir = Self::create_stream_temp_dir(&stream_id)?;
        let hls_ctx = HlsOutputContext::create_segment(temp_dir.path(), streams.as_ref(), 0)?;

        Ok(Self {
            stream_id,
            streams,
            state: Mutex::new(SegmentState::new(temp_dir, hls_ctx)),
            segment_duration,
        })
    }

    async fn emit_segment_complete(live_id: String, segment_path: PathBuf) {
        dispatcher::singleton()
            .await
            .send(SessionEvent::SegmentComplete {
                live_id,
                path: segment_path,
            });
    }

    fn create_stream_temp_dir(stream_id: &str) -> Result<TempDir> {
        let sanitized = normalize_component(stream_id);
        let prefix = format!("livestream-rs-segment-{}-", sanitized);

        Ok(Builder::new().prefix(&prefix).tempdir()?)
    }

    async fn write_packet(&self, mut packet: Packet) -> Result<()> {
        let completed_segment_path = {
            let mut state = self.state.lock().await;
            let mut completed: Option<PathBuf> = None;

            if state.should_rollover(&packet, self.segment_duration) {
                completed = Some(state.rollover(self.streams.as_ref())?);
            }

            if let Some(hls_ctx) = state.hls_ctx.as_mut() {
                packet.rescale_ts_for_stream(self.streams.as_ref(), hls_ctx)?;
                packet.write(hls_ctx)?;
            }

            completed
        };

        if let Some(path) = completed_segment_path {
            Self::emit_segment_complete(self.stream_id.clone(), path).await;
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl MiddlewareTrait for SegmentMiddleware {
    type Context = UnifiedPacketContext;

    async fn send(&self, ctx: Self::Context) -> Result<Self::Context> {
        let stream_id = ctx.id();
        if stream_id != self.stream_id {
            warn!(expected = %self.stream_id, got = %stream_id, "Stream id mismatch in SegmentMiddleware");
            return Ok(ctx);
        }

        match ctx.payload() {
            UnifiedPacket::AVPacket(packet) => {
                self.write_packet(packet.clone()).await?;
            }
            UnifiedPacket::FlvTag(tag) => {
                match tag.to_packet_ref(self.streams.as_ref()) {
                    Ok(packet) => {
                        self.write_packet(packet).await?;
                    }
                    Err(e) => {
                        metrics::get_metrics().record_pipeline_error("segment_flv_convert");
                        warn!(stream_id = %stream_id, error = %e, "Failed to convert FLV tag to packet with stream mapping");
                    }
                }
            }
        }

        Ok(ctx)
    }
}
