use std::path::PathBuf;
use std::sync::atomic::Ordering;

use crossfire::{AsyncRx, mpsc};
use tokio::task::block_in_place;
use tracing::{instrument, warn};

use crate::ingest::session::{StreamAdapter, WorkerContext};
use crate::media::output::FlvPacket;
use crate::telemetry::metrics;

/// Internal RTMP ingest payload model consumed by the RTMP worker.
///
/// Responsibilities:
/// - Carry typed media/control payloads between RTMP server and adapter loop.
/// - Preserve timestamp/payload data required by mux/segment logic.
///
/// Out of scope:
/// - No transport I/O ownership.
/// - No lifecycle side effects.
pub enum RtmpTag {
    Audio {
        tag: bytes::Bytes,
        timestamp: u32,
        data_len: usize,
    },
    Video {
        tag: bytes::Bytes,
        timestamp: u32,
        payload: bytes::Bytes,
    },
    Header {
        tag: bytes::Bytes,
    },
    PublishFinished,
}

/// RTMP-side adapter for one ingest worker session.
///
/// Responsibilities:
/// - Consume `RtmpTag` stream and feed FLV/HLS mux segmentation.
/// - Emit segment-complete notifications via `WorkerLifecycle`.
///
/// Out of scope:
/// - No manager actor state mutation.
/// - No callback/network side effects beyond channel sends.
pub struct RtmpAdapter {
    rx: AsyncRx<mpsc::List<RtmpTag>>,
}

impl RtmpAdapter {
    pub fn new(rx: AsyncRx<mpsc::List<RtmpTag>>) -> Self {
        Self { rx }
    }

    fn on_audio_data(
        &mut self,
        ctx: &mut WorkerContext,
        muxer: &mut StreamMuxer,
        timestamp: u32,
        tag: bytes::Bytes,
        data_len: usize,
    ) {
        let mut completed_segment: Option<(String, PathBuf)> = None;

        let labels = metrics::protocol_labels("rtmp");
        metrics::get_metrics().add_network_bytes_in(data_len as u64, &labels);
        metrics::get_metrics().add_ingest_packets(1, &labels);

        if let Some(path) = muxer.segmenter.on_audio_tag(&tag, timestamp) {
            completed_segment = Some((ctx.live_id.clone(), path));
        }
        let _ = ctx.flv_packet_tx.send(FlvPacket::Data {
            live_id: ctx.live_id.clone(),
            data: tag,
        });

        if let Some((_live_id, path)) = completed_segment {
            self.notify_segment_complete(ctx, path);
        }
    }

    fn on_video_data(
        &mut self,
        ctx: &mut WorkerContext,
        muxer: &mut StreamMuxer,
        timestamp: u32,
        tag: bytes::Bytes,
        payload: &[u8],
    ) {
        let mut completed_segment: Option<(String, PathBuf)> = None;

        let labels = metrics::protocol_labels("rtmp");
        metrics::get_metrics().add_network_bytes_in(payload.len() as u64, &labels);
        metrics::get_metrics().add_ingest_packets(1, &labels);

        if let Some(path) = muxer.segmenter.on_video_tag(&tag, timestamp, payload) {
            completed_segment = Some((ctx.live_id.clone(), path));
        }
        let _ = ctx.flv_packet_tx.send(FlvPacket::Data {
            live_id: ctx.live_id.clone(),
            data: tag,
        });

        if let Some((_live_id, path)) = completed_segment {
            self.notify_segment_complete(ctx, path);
        }
    }

    fn notify_segment_complete(&self, ctx: &mut WorkerContext, path: PathBuf) {
        ctx.lifecycle.notify_segment_complete(&path);
    }

    fn handle_rtmp_tag(
        &mut self,
        ctx: &mut WorkerContext,
        muxer: &mut StreamMuxer,
        tag: RtmpTag,
    ) -> bool {
        match tag {
            RtmpTag::Header { tag } => {
                let _ = ctx.flv_packet_tx.send(FlvPacket::Data {
                    live_id: ctx.live_id.clone(),
                    data: tag,
                });
                true
            }
            RtmpTag::Audio {
                tag,
                timestamp,
                data_len,
            } => {
                self.on_audio_data(ctx, muxer, timestamp, tag, data_len);
                true
            }
            RtmpTag::Video {
                tag,
                timestamp,
                payload,
            } => {
                self.on_video_data(ctx, muxer, timestamp, tag, &payload);
                true
            }
            RtmpTag::PublishFinished => false,
        }
    }
}

impl StreamAdapter for RtmpAdapter {
    #[instrument(name = "ingest.rtmp_worker.run", skip(self, ctx), fields(stream.live_id = %ctx.live_id))]
    async fn run(&mut self, ctx: &mut WorkerContext) -> anyhow::Result<()> {
        let segment_duration_ms = (ctx.stream_info.segment_duration().max(1) * 1000) as u32;
        let mut muxer = StreamMuxer::new(segment_duration_ms)?;

        ctx.lifecycle.notify_stream_started();

        let mut check_interval = tokio::time::interval(std::time::Duration::from_millis(100));

        loop {
            tokio::select! {
                _ = check_interval.tick() => {
                    if ctx.stop_signal.load(Ordering::SeqCst) {
                        tracing::info!("Stop signal received, exiting rtmp worker loop");
                        break;
                    }
                }
                recv_result = self.rx.recv() => {
                    let Ok(tag) = recv_result else {
                        break;
                    };

                    if !self.handle_rtmp_tag(ctx, &mut muxer, tag) {
                        break;
                    }
                }
            }
        }

        if let Some(path) = muxer.segmenter.flush_final_segment() {
            self.notify_segment_complete(ctx, path);
        }

        Ok(())
    }
}

pub fn make_rtmp_tag(tag_type: u8, timestamp: u32, payload: &[u8]) -> bytes::Bytes {
    let data_size = payload.len() as u32;
    let mut out = Vec::with_capacity(11 + payload.len() + 4);

    out.push(tag_type);
    out.push(((data_size >> 16) & 0xFF) as u8);
    out.push(((data_size >> 8) & 0xFF) as u8);
    out.push((data_size & 0xFF) as u8);

    out.push(((timestamp >> 16) & 0xFF) as u8);
    out.push(((timestamp >> 8) & 0xFF) as u8);
    out.push((timestamp & 0xFF) as u8);
    out.push(((timestamp >> 24) & 0xFF) as u8);

    out.extend_from_slice(&[0x00, 0x00, 0x00]);
    out.extend_from_slice(payload);

    let prev_size = 11 + data_size;
    out.push(((prev_size >> 24) & 0xFF) as u8);
    out.push(((prev_size >> 16) & 0xFF) as u8);
    out.push(((prev_size >> 8) & 0xFF) as u8);
    out.push((prev_size & 0xFF) as u8);

    bytes::Bytes::from(out)
}

/// Lightweight coordinator around FLV segmentation policy.
///
/// Responsibilities:
/// - Encapsulate segmenter creation and ownership for RTMP adapter run.
///
/// Out of scope:
/// - No I/O loop orchestration or lifecycle signaling.
struct StreamMuxer {
    segmenter: FlvSegmenter,
}

impl StreamMuxer {
    fn new(segment_duration_ms: u32) -> anyhow::Result<Self> {
        Ok(Self {
            segmenter: FlvSegmenter::new(segment_duration_ms)?,
        })
    }
}

/// Stateful FLV segment builder backed by temporary files.
///
/// Responsibilities:
/// - Accumulate FLV tags and roll segments on keyframe + duration policy.
/// - Persist completed segments and expose their paths.
///
/// Out of scope:
/// - No stream control decisions.
/// - No telemetry or manager-state interaction.
struct FlvSegmenter {
    temp_dir: tempfile::TempDir,
    segment_duration_ms: u32,
    segment_id: u64,
    segment_start_ts: Option<u32>,
    segment_bytes: Vec<u8>,
}

impl FlvSegmenter {
    fn new(segment_duration_ms: u32) -> anyhow::Result<Self> {
        let temp_dir = tempfile::tempdir()?;

        let mut segment_bytes = Vec::with_capacity(256 * 1024);
        segment_bytes.extend_from_slice(&[
            0x46, 0x4C, 0x56, 0x01, 0x05, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00, 0x00,
        ]);

        Ok(Self {
            temp_dir,
            segment_duration_ms,
            segment_id: 1,
            segment_start_ts: None,
            segment_bytes,
        })
    }

    fn on_audio_tag(&mut self, tag: &bytes::Bytes, timestamp: u32) -> Option<PathBuf> {
        if self.segment_start_ts.is_none() {
            self.segment_start_ts = Some(timestamp);
        }
        self.segment_bytes.extend_from_slice(tag);
        None
    }

    fn on_video_tag(
        &mut self,
        tag: &bytes::Bytes,
        timestamp: u32,
        payload: &[u8],
    ) -> Option<PathBuf> {
        let should_roll = self.should_roll_segment(timestamp, payload);
        if should_roll {
            let completed = self.persist_current_segment();
            self.start_new_segment(timestamp);
            self.segment_bytes.extend_from_slice(tag);
            return completed;
        }

        if self.segment_start_ts.is_none() {
            self.segment_start_ts = Some(timestamp);
        }
        self.segment_bytes.extend_from_slice(tag);
        None
    }

    fn flush_final_segment(&mut self) -> Option<PathBuf> {
        self.persist_current_segment()
    }

    fn should_roll_segment(&self, timestamp: u32, payload: &[u8]) -> bool {
        let Some(start_ts) = self.segment_start_ts else {
            return false;
        };

        let is_keyframe = payload
            .first()
            .map(|byte| (byte >> 4) == 1)
            .unwrap_or(false);

        is_keyframe && timestamp.saturating_sub(start_ts) >= self.segment_duration_ms
    }

    fn start_new_segment(&mut self, timestamp: u32) {
        self.segment_id += 1;
        self.segment_start_ts = Some(timestamp);
        self.segment_bytes.clear();
        self.segment_bytes.extend_from_slice(&[
            0x46, 0x4C, 0x56, 0x01, 0x05, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00, 0x00,
        ]);
    }

    fn persist_current_segment(&mut self) -> Option<PathBuf> {
        if self.segment_bytes.len() <= 13 {
            return None;
        }

        let file_name = format!("{:06}.flv", self.segment_id);
        let path = self.temp_dir.path().join(file_name);

        if let Err(err) = block_in_place(|| std::fs::write(&path, &self.segment_bytes)) {
            warn!(error = %err, path = %path.display(), "Failed to write RTMP segment file");
            return None;
        }

        Some(path)
    }
}
