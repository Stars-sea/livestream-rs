use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use ffmpeg_sys_next::{AV_NOPTS_VALUE, AVRational};

use crate::infra::media::context::HlsOutputContext;
use crate::infra::media::packet::Packet;
use crate::infra::media::stream::StreamCollection;

use super::ingest::IngestState;

pub(super) struct CompletedSegment {
    pub(super) path: PathBuf,
    pub(super) duration: Duration,
}

#[derive(Default)]
struct SegmentTimeline {
    started_at: Option<Duration>,
    ended_at: Option<Duration>,
}

impl SegmentTimeline {
    fn reset(&mut self) {
        self.started_at = None;
        self.ended_at = None;
    }

    fn observe_packet(&mut self, packet: &Packet, streams: &dyn StreamCollection) -> Result<()> {
        let Some(started_at) = packet_start_time(packet, streams)? else {
            return Ok(());
        };
        let ended_at = packet_end_time(packet, streams)?.unwrap_or(started_at);

        self.started_at.get_or_insert(started_at);
        self.ended_at = Some(
            self.ended_at
                .map_or(ended_at, |current| current.max(ended_at)),
        );
        Ok(())
    }

    fn elapsed(&self) -> Option<Duration> {
        Some(self.ended_at? - self.started_at?)
    }
}

pub(super) struct HlsSegmenter {
    hls_ctx: Option<HlsOutputContext>,
    segment_root: PathBuf,
    next_segment_id: u64,
    segment_duration: Duration,
    timeline: SegmentTimeline,
}

impl HlsSegmenter {
    pub(super) fn new(segment_root: PathBuf, segment_duration: Duration) -> Self {
        Self {
            hls_ctx: None,
            segment_root,
            next_segment_id: 0,
            segment_duration,
            timeline: SegmentTimeline::default(),
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
        self.timeline.reset();
        Ok(())
    }

    fn complete_active_segment(&mut self) -> Option<CompletedSegment> {
        let path = self.hls_ctx.as_ref()?.path().to_path_buf();
        let duration = self.timeline.elapsed().unwrap_or_default();
        self.hls_ctx.take();
        self.timeline.reset();
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

        let should_rollover = packet.is_key_frame()
            && self
                .timeline
                .elapsed()
                .is_some_and(|elapsed| elapsed >= self.segment_duration);

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
        self.timeline.observe_packet(&packet, streams)?;

        if let Some(hls_ctx) = self.hls_ctx.as_mut() {
            packet.rescale_ts_for_stream(streams, hls_ctx)?;
            packet.write(hls_ctx)?;
        }
        Ok(())
    }
}

fn packet_start_time(packet: &Packet, streams: &dyn StreamCollection) -> Result<Option<Duration>> {
    packet_timestamp_to_duration(
        packet.stream_idx(),
        packet.pts().or_else(|| packet.dts()),
        streams,
    )
}

fn packet_end_time(packet: &Packet, streams: &dyn StreamCollection) -> Result<Option<Duration>> {
    let Some(start) = packet_start_time(packet, streams)? else {
        return Ok(None);
    };

    let Some(packet_duration) = packet.duration() else {
        return Ok(Some(start));
    };

    let Some(delta) = packet_duration_to_duration(packet_duration, packet.stream_idx(), streams)?
    else {
        return Ok(Some(start));
    };

    Ok(Some(start.saturating_add(delta)))
}

fn packet_timestamp_to_duration(
    stream_idx: usize,
    timestamp: Option<i64>,
    streams: &dyn StreamCollection,
) -> Result<Option<Duration>> {
    let Some(timestamp) = timestamp else {
        return Ok(None);
    };
    let Some(time_base) = streams.time_base(stream_idx) else {
        anyhow::bail!("Stream {} not found", stream_idx);
    };

    Ok(scale_to_duration(timestamp, time_base))
}

fn packet_duration_to_duration(
    value: i64,
    stream_idx: usize,
    streams: &dyn StreamCollection,
) -> Result<Option<Duration>> {
    let Some(time_base) = streams.time_base(stream_idx) else {
        anyhow::bail!("Stream {} not found", stream_idx);
    };

    Ok(scale_to_duration(value, time_base))
}

fn scale_to_duration(value: i64, time_base: AVRational) -> Option<Duration> {
    if value == AV_NOPTS_VALUE || value < 0 || time_base.num <= 0 || time_base.den <= 0 {
        return None;
    }

    let nanos = (value as u128)
        .checked_mul(time_base.num as u128)?
        .checked_mul(1_000_000_000)?
        .checked_div(time_base.den as u128)?;

    Some(Duration::from_nanos(nanos.min(u64::MAX as u128) as u64))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ffmpeg_sys_next::AVRational;
    use rml_rtmp::sessions::StreamMetadata;

    use super::{SegmentTimeline, packet_end_time};
    use crate::infra::media::packet::Packet;

    #[test]
    fn timeline_uses_media_timestamps_for_elapsed() {
        let streams = StreamMetadata::new();
        let mut timeline = SegmentTimeline::default();

        let first = make_packet(1_000, AVRational { num: 1, den: 1000 }, true, 0);
        let second = make_packet(4_600, AVRational { num: 1, den: 1000 }, false, 0);

        timeline.observe_packet(&first, &streams).unwrap();
        timeline.observe_packet(&second, &streams).unwrap();

        assert_eq!(timeline.elapsed(), Some(Duration::from_millis(3600)));
    }

    #[test]
    fn packet_end_time_includes_packet_duration() {
        let streams = StreamMetadata::new();
        let mut packet = make_packet(2_000, AVRational { num: 1, den: 1000 }, true, 0);
        set_packet_duration(&mut packet, 250);

        let end = packet_end_time(&packet, &streams).unwrap();

        assert_eq!(end, Some(Duration::from_millis(2250)));
    }

    #[test]
    fn timeline_ignores_packets_without_timestamps() {
        let streams = StreamMetadata::new();
        let mut timeline = SegmentTimeline::default();
        let mut packet = Packet::alloc().unwrap();

        unsafe {
            (*packet.ptr()).stream_index = 0;
        }

        timeline.observe_packet(&packet, &streams).unwrap();

        assert_eq!(timeline.elapsed(), None);
    }

    fn make_packet(
        timestamp: u32,
        time_base: AVRational,
        is_keyframe: bool,
        stream_idx: usize,
    ) -> Packet {
        let mut packet = Packet::alloc().unwrap();

        unsafe {
            let pkt_ref = packet.ptr();
            (*pkt_ref).stream_index = stream_idx as i32;
            (*pkt_ref).pts = timestamp as i64;
            (*pkt_ref).dts = timestamp as i64;

            if is_keyframe {
                (*pkt_ref).flags |= ffmpeg_sys_next::AV_PKT_FLAG_KEY;
            }
        }

        packet.rescale_ts(AVRational { num: 1, den: 1000 }, time_base);
        packet
    }

    fn set_packet_duration(packet: &mut Packet, duration: i64) {
        unsafe {
            (*packet.ptr()).duration = duration;
        }
    }
}
