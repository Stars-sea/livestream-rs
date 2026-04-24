use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::infra::media::context::HlsOutputContext;
use crate::infra::media::packet::Packet;
use crate::infra::media::stream::StreamCollection;

use super::ingest::IngestState;

pub(super) struct CompletedSegment {
    pub(super) path: PathBuf,
    pub(super) duration: Duration,
}

pub(super) struct HlsSegmenter {
    hls_ctx: Option<HlsOutputContext>,
    segment_root: PathBuf,
    next_segment_id: u64,
    segment_started_at: Instant,
    segment_duration: Duration,
}

impl HlsSegmenter {
    pub(super) fn new(segment_root: PathBuf, segment_duration: Duration) -> Self {
        Self {
            hls_ctx: None,
            segment_root,
            next_segment_id: 0,
            segment_started_at: Instant::now(),
            segment_duration,
        }
    }

    fn start_next_segment(
        &mut self,
        streams: &dyn StreamCollection,
        ingest_state: &IngestState,
    ) -> Result<()> {
        let mut next_ctx =
            HlsOutputContext::create_segment(&self.segment_root, streams, self.next_segment_id)?;
        ingest_state.apply_codec_extradata(&next_ctx)?;
        next_ctx.write_header()?;
        self.hls_ctx = Some(next_ctx);
        self.segment_started_at = Instant::now();
        Ok(())
    }

    fn complete_active_segment(&mut self) -> Option<CompletedSegment> {
        let path = self.hls_ctx.as_ref()?.path().to_path_buf();
        let duration = self.segment_started_at.elapsed();
        self.hls_ctx.take();
        Some(CompletedSegment { path, duration })
    }

    pub(super) fn rollover_if_needed(
        &mut self,
        packet: &Packet,
        streams: &dyn StreamCollection,
        ingest_state: &IngestState,
    ) -> Result<Option<CompletedSegment>> {
        if self.hls_ctx.is_none() {
            self.start_next_segment(streams, ingest_state)?;
            return Ok(None);
        }

        let should_rollover =
            packet.is_key_frame() && self.segment_started_at.elapsed() >= self.segment_duration;

        if !should_rollover {
            return Ok(None);
        }

        let completed = self.complete_active_segment();
        self.next_segment_id = self.next_segment_id.saturating_add(1);
        self.start_next_segment(streams, ingest_state)?;
        Ok(completed)
    }

    pub(super) fn finish_active_segment(&mut self) -> Option<CompletedSegment> {
        self.complete_active_segment()
    }

    pub(super) fn write_packet(
        &mut self,
        mut packet: Packet,
        streams: &dyn StreamCollection,
    ) -> Result<()> {
        if let Some(hls_ctx) = self.hls_ctx.as_mut() {
            packet.rescale_ts_for_stream(streams, hls_ctx)?;
            packet.write(hls_ctx)?;
        }
        Ok(())
    }
}
