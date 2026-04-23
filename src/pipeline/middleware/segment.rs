use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::Mutex;
use tracing::warn;

use crate::abstraction::{MiddlewareTrait, PipeContextTrait};
use crate::infra::media::packet::{FlvTag, Packet, UnifiedPacket};
use crate::infra::media::stream::StreamCollection;
use crate::metric_pipeline_error;
use crate::pipeline::UnifiedPacketContext;

mod events;
mod ingest;
mod playlist;
mod segmenter;
mod workspace;

use events::EventDispatchPlan;
use ingest::IngestState;
use playlist::PlaylistState;
use segmenter::{CompletedSegment, HlsSegmenter};
use workspace::SegmentWorkspace;

struct SegmentState {
    segmenter: HlsSegmenter,
    ingest: IngestState,
    playlist: PlaylistState,
    workspace: SegmentWorkspace,
}

impl SegmentState {
    fn new(stream_id: &str, segment_duration: Duration, cache_dir: &str) -> Result<Self> {
        let workspace = SegmentWorkspace::new(stream_id, cache_dir)?;
        let segment_root = workspace.segment_root().to_path_buf();
        let playlist_path = workspace.playlist_path();

        Ok(Self {
            segmenter: HlsSegmenter::new(segment_root, segment_duration),
            ingest: IngestState::default(),
            playlist: PlaylistState::new(playlist_path, segment_duration),
            // Keep workspace last so TempDir outlives the FFmpeg output context during drop.
            workspace,
        })
    }

    fn packetize_flv_tag(
        &mut self,
        tag: &FlvTag,
        streams: &dyn StreamCollection,
    ) -> Result<Vec<Packet>> {
        self.ingest.packetize_flv_tag(tag, streams)
    }

    fn write_packet(
        &mut self,
        packet: Packet,
        streams: &dyn StreamCollection,
    ) -> Result<Option<EventDispatchPlan>> {
        self.ingest.start_packet_ingest();

        let updates = self
            .segmenter
            .rollover_if_needed(&packet, streams, &self.ingest)?
            .map(|completed| self.record_completed_segment(completed, false))
            .transpose()?;

        self.segmenter.write_packet(packet, streams)?;
        Ok(updates)
    }

    fn finalize(&mut self) -> Result<Option<EventDispatchPlan>> {
        if let Some(completed) = self.segmenter.finish_active_segment() {
            return self.record_completed_segment(completed, true).map(Some);
        }

        if !self.playlist.has_entries() {
            return Ok(None);
        }

        let playlist_path = self.playlist.write_snapshot(true)?;
        Ok(Some(EventDispatchPlan::playlist_only(playlist_path, true)))
    }

    fn record_completed_segment(
        &mut self,
        completed: CompletedSegment,
        is_final: bool,
    ) -> Result<EventDispatchPlan> {
        let staged_path = self.workspace.stage_segment(&completed.path)?;
        let filename = staged_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                anyhow::anyhow!("Invalid staged segment filename: {}", staged_path.display())
            })?
            .to_string();

        self.playlist.push_segment(filename, completed.duration);
        let playlist_path = self.playlist.write_snapshot(is_final)?;

        Ok(EventDispatchPlan::completed_segment(
            staged_path,
            playlist_path,
            is_final,
        ))
    }
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
        let state = SegmentState::new(&stream_id, segment_duration, &segment_cachedir)?;

        Ok(Self {
            stream_id,
            streams,
            state: Mutex::new(state),
        })
    }

    fn emit_updates(&self, updates: Option<EventDispatchPlan>) {
        if let Some(updates) = updates {
            updates.dispatch(&self.stream_id);
        }
    }

    async fn write_packet(&self, packet: Packet) -> Result<()> {
        let updates = {
            let mut state = self.state.lock().await;
            state.write_packet(packet, self.streams.as_ref())?
        };

        self.emit_updates(updates);
        Ok(())
    }

    async fn process_flv_tag(&self, stream_id: &str, tag: &FlvTag) -> Result<()> {
        let packets = {
            let mut state = self.state.lock().await;
            state.packetize_flv_tag(tag, self.streams.as_ref())
        };

        match packets {
            Ok(packets) => {
                for packet in packets {
                    self.write_packet(packet).await?;
                }
            }
            Err(error) => {
                metric_pipeline_error!("segment_flv_convert");
                warn!(stream_id = %stream_id, error = %error, "Failed to convert FLV tag to packet with stream mapping");
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
        if stream_id != self.stream_id {
            warn!(expected = %self.stream_id, got = %stream_id, "Stream id mismatch in SegmentMiddleware");
            return Ok(ctx);
        }

        match ctx.payload() {
            UnifiedPacket::AVPacket(packet) => {
                self.write_packet(packet.clone()).await?;
            }
            UnifiedPacket::FlvTag(tag) => {
                self.process_flv_tag(stream_id, tag).await?;
            }
        }

        Ok(ctx)
    }

    async fn close(&self) -> Result<()> {
        let updates = {
            let mut state = self.state.lock().await;
            state.finalize()?
        };

        self.emit_updates(updates);
        Ok(())
    }
}
