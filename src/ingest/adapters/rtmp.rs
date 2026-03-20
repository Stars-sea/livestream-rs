use std::path::PathBuf;

use tokio::sync::mpsc;
use tokio::task::block_in_place;
use tracing::{Span, instrument, warn};

use crate::ingest::events::StreamMessage;
use crate::ingest::session::{StreamAdapter, WorkerContext};
use crate::media::output::FlvPacket;
use crate::telemetry::metrics;

/// Represents different types of FLV/RTMP tags parsed from the incoming stream.
/// These tags encapsulate media payload and timestamp data for multiplexing.
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

/// The RTMP adapter implementation of `StreamAdapter`.
/// Responsible for receiving `RtmpTag` inputs and converting them into stream events
/// to be broadcast over the shared worker context.
pub struct RtmpAdapter {
    rx: mpsc::UnboundedReceiver<RtmpTag>,
}

impl RtmpAdapter {
    pub fn new(rx: mpsc::UnboundedReceiver<RtmpTag>) -> Self {
        Self { rx }
    }

    #[instrument(name = "ingest.rtmp_worker.audio_data", skip(self, ctx, muxer, tag), fields(stream.protocol = "rtmp", stream.live_id = %ctx.live_id, stream.timestamp = timestamp, media.bytes = data_len))]
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

        if let Some((live_id, path)) = completed_segment {
            self.notify_segment_complete(ctx, &live_id, path);
        }
    }

    #[instrument(name = "ingest.rtmp_worker.video_data", skip(self, ctx, muxer, tag, payload), fields(stream.protocol = "rtmp", stream.live_id = %ctx.live_id, stream.timestamp = timestamp, media.bytes = payload.len()))]
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

        if let Some((live_id, path)) = completed_segment {
            self.notify_segment_complete(ctx, &live_id, path);
        }
    }

    fn notify_segment_complete(&self, ctx: &mut WorkerContext, live_id: &str, path: PathBuf) {
        let event = (
            StreamMessage::segment_complete(live_id, &path),
            Span::current(),
        );
        if let Err(err) = ctx.stream_msg_tx.send(event) {
            warn!(error = %err, stream.live_id = %live_id, "Failed to emit RTMP segment_complete event");
        }
    }
}

impl StreamAdapter for RtmpAdapter {
    #[instrument(name = "ingest.rtmp_worker.run", skip(self, ctx), fields(stream.live_id = %ctx.live_id))]
    async fn run(&mut self, ctx: &mut WorkerContext) -> anyhow::Result<()> {
        let segment_duration_ms = (ctx.stream_info.segment_duration().max(1) * 1000) as u32;
        let mut muxer = StreamMuxer::new(segment_duration_ms)?;

        ctx.lifecycle.notify_stream_started(&ctx.stream_msg_tx);

        let mut check_interval = tokio::time::interval(std::time::Duration::from_millis(100));

        loop {
            tokio::select! {
                _ = check_interval.tick() => {
                    if ctx.stop_signal.load(std::sync::atomic::Ordering::SeqCst) {
                        tracing::info!("Stop signal received, exiting rtmp worker loop");
                        break;
                    }
                }
                tag_opt = self.rx.recv() => {
                    match tag_opt {
                        Some(tag) => {
                            match tag {
                                RtmpTag::Header { tag } => {
                                    let _ = ctx.flv_packet_tx.send(FlvPacket::Data {
                                        live_id: ctx.live_id.clone(),
                                        data: tag,
                                    });
                                }
                                RtmpTag::Audio {
                                    tag,
                                    timestamp,
                                    data_len,
                                } => {
                                    self.on_audio_data(ctx, &mut muxer, timestamp, tag, data_len);
                                }
                                RtmpTag::Video {
                                    tag,
                                    timestamp,
                                    payload,
                                } => {
                                    self.on_video_data(ctx, &mut muxer, timestamp, tag, &payload);
                                }
                                RtmpTag::PublishFinished => {
                                    break;
                                }
                            }
                        }
                        None => {
                            break;
                        }
                    }
                }
            }
        }

        if let Some(path) = muxer.segmenter.flush_final_segment() {
            self.notify_segment_complete(ctx, &ctx.live_id.clone(), path);
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

/// Coordinates multiplexing of continuous stream data into discrete chunks or packets.
/// Utilizes a `FlvSegmenter` under the hood to break the stream into manageable segments.
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

/// Breaks down an ongoing FLV stream into sequential segments based on a set duration.
/// Caches the segment bytes temporarily until they are finalized and flushed.
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
