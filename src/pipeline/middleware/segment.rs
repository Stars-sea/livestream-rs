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
use crate::infra::media::packet::{FlvTag, FlvTagPacketizer, Packet, UnifiedPacket};
use crate::infra::media::stream::StreamCollection;
use crate::pipeline::UnifiedPacketContext;
use crate::pipeline::normalize::normalize_component;
use crate::telemetry::metrics;

/// The ingest state tracks how the stream is being ingested,
/// allowing for optimizations like lazy initialization of the packetizer for RTMP or direct forwarding for SRT.
enum IngestState {
    Pending,
    /// Direct packet-driven ingest (e.g. SRT) where packets are directly forwarded to the segmenter without FLV parsing.
    PacketDriven,
    /// FLV tag-driven ingest (e.g. RTMP) where we need to parse FLV tags, extract codec extradata, and convert to packets.
    FlvDriven(FlvTagPacketizer),
}

impl Default for IngestState {
    fn default() -> Self {
        Self::Pending
    }
}

impl IngestState {
    fn apply_codec_extradata(&self, ctx: &HlsOutputContext) -> Result<()> {
        if let Self::FlvDriven(packetizer) = self {
            packetizer.apply_codec_extradata(ctx)?;
        }
        Ok(())
    }
}

/// For managing HLS output contexts and handling time-based segmenting logic.
struct HlsSegmenter {
    hls_ctx: Option<HlsOutputContext>,
    temp_dir: TempDir,
    segment_id: u64,
    segment_started_at: Instant,
    segment_duration: Duration,
}

impl HlsSegmenter {
    fn new(temp_dir: TempDir, segment_duration: Duration) -> Self {
        Self {
            hls_ctx: None,
            temp_dir,
            segment_id: 0,
            segment_started_at: Instant::now(),
            segment_duration,
        }
    }

    fn init_next_segment(
        &mut self,
        streams: &dyn StreamCollection,
        ingest_state: &IngestState,
    ) -> Result<()> {
        let mut next_ctx =
            HlsOutputContext::create_segment(self.temp_dir.path(), streams, self.segment_id)?;
        ingest_state.apply_codec_extradata(&next_ctx)?;
        next_ctx.write_header()?;
        self.hls_ctx = Some(next_ctx);
        self.segment_started_at = Instant::now();
        Ok(())
    }

    fn check_and_rollover(
        &mut self,
        packet: &Packet,
        streams: &dyn StreamCollection,
        ingest_state: &IngestState,
    ) -> Result<Option<PathBuf>> {
        if self.hls_ctx.is_none() {
            self.init_next_segment(streams, ingest_state)?;
            return Ok(None);
        }

        let should_rollover =
            packet.is_key_frame() && self.segment_started_at.elapsed() >= self.segment_duration;

        if should_rollover {
            let completed_path = self.hls_ctx.as_ref().unwrap().path().clone();
            self.segment_id = self.segment_id.saturating_add(1);
            self.init_next_segment(streams, ingest_state)?;
            return Ok(Some(completed_path));
        }

        Ok(None)
    }

    fn write_packet(&mut self, mut packet: Packet, streams: &dyn StreamCollection) -> Result<()> {
        if let Some(hls_ctx) = self.hls_ctx.as_mut() {
            packet.rescale_ts_for_stream(streams, hls_ctx)?;
            packet.write(hls_ctx)?;
        }
        Ok(())
    }
}

impl Drop for HlsSegmenter {
    fn drop(&mut self) {
        // Drop output context first so file handles are closed before TempDir cleanup.
        self.hls_ctx.take();

        // Touch temp_dir to make intent explicit and silence unused-field warnings.
        let _ = self.temp_dir.path();
    }
}

struct SegmentState {
    segmenter: HlsSegmenter,
    ingest: IngestState,
}

pub struct SegmentMiddleware {
    stream_id: String,
    streams: Arc<dyn StreamCollection + Send + Sync>,
    state: Mutex<SegmentState>,
}

impl SegmentMiddleware {
    pub fn new(
        stream_id: String,
        streams: Arc<dyn StreamCollection + Send + Sync>,
        segment_duration: Duration,
        segment_cachedir: String,
    ) -> Result<Self> {
        let temp_dir = Self::create_stream_temp_dir(&stream_id, &segment_cachedir)?;

        Ok(Self {
            stream_id,
            streams,
            state: Mutex::new(SegmentState {
                segmenter: HlsSegmenter::new(temp_dir, segment_duration),
                ingest: IngestState::default(),
            }),
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

    fn create_stream_temp_dir(stream_id: &str, cache_dir: &str) -> Result<TempDir> {
        let sanitized = normalize_component(stream_id);
        let prefix = format!("livestream-rs-segment-{}-", sanitized);

        if cache_dir.trim().is_empty() {
            return Ok(Builder::new().prefix(&prefix).tempdir()?);
        }

        let root = PathBuf::from(cache_dir);
        std::fs::create_dir_all(&root)?;
        Ok(Builder::new().prefix(&prefix).tempdir_in(root)?)
    }

    async fn write_packet(&self, packet: Packet) -> Result<()> {
        let completed_segment_path = {
            let mut state = self.state.lock().await;
            let SegmentState { segmenter, ingest } = &mut *state;

            if matches!(ingest, IngestState::Pending) {
                *ingest = IngestState::PacketDriven;
            }

            let completed = segmenter.check_and_rollover(&packet, self.streams.as_ref(), ingest)?;

            segmenter.write_packet(packet, self.streams.as_ref())?;

            completed
        };

        if let Some(path) = completed_segment_path {
            Self::emit_segment_complete(self.stream_id.clone(), path).await;
        }

        Ok(())
    }

    async fn packetize_flv_tag(&self, tag: &FlvTag) -> Result<Vec<Packet>> {
        let mut state = self.state.lock().await;

        if matches!(state.ingest, IngestState::Pending) {
            state.ingest = IngestState::FlvDriven(FlvTagPacketizer::new());
        }

        if let IngestState::FlvDriven(packetizer) = &mut state.ingest {
            packetizer.packetize(tag, self.streams.as_ref())
        } else {
            anyhow::bail!("Mixed stream ingest types detected: expected FLV-driven state.");
        }
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
            UnifiedPacket::FlvTag(tag) => match self.packetize_flv_tag(tag).await {
                Ok(packets) => {
                    for packet in packets {
                        self.write_packet(packet).await?;
                    }
                }
                Err(e) => {
                    metrics::get_metrics().record_pipeline_error("segment_flv_convert");
                    warn!(stream_id = %stream_id, error = %e, "Failed to convert FLV tag to packet with stream mapping");
                }
            },
        }

        Ok(ctx)
    }
}
