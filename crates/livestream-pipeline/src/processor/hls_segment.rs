//! HlsSegmenter — HLS segment production via FFmpeg TS muxer.
//!
//! Uses `HlsOutputContext` (custom AVIO → in-memory `Vec<u8>` buffer)
//! from `livestream-media`. Segment rollover flushes the buffer to disk
//! and stages the file for MinIoSink upload.
//!
//! Always active (`should_process()` returns `true`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use livestream_codec::{EncodedPacket, SegmentConfig, TsSegment};
use livestream_core::{
    pad::{PadReceiver, PadSender},
    traits::{Node, Processor},
    types::CodecParams,
};
use livestream_media::ffmpeg_sys_next::AVRational;
use livestream_media::{
    context::HlsOutputContext, convert::IntoAvPacket, stream::StaticStreamCollection,
};
use livestream_telemetry::metric_pipeline_error;
use parking_lot::Mutex;
use tempfile::TempDir;

// ── SegmentWorkspace ──

/// Manages temporary segment storage.
///
/// Layout:
///   {cache_dir}/livestream-rs-segment-{stream_id}-XXXXXX/  ← TempDir (auto-cleaned)
///   {cache_dir}/livestream-rs-artifacts/{stream_id}/         ← staging for upload
pub struct SegmentWorkspace {
    segment_dir: TempDir,
    upload_dir: PathBuf,
}

impl SegmentWorkspace {
    pub fn new(stream_id: &str, cfg: &SegmentConfig) -> Result<Self> {
        let cache_root = if cfg.cache_dir.trim().is_empty() {
            std::env::temp_dir()
        } else {
            PathBuf::from(&cfg.cache_dir)
        };
        fs::create_dir_all(&cache_root)?;

        let segment_dir = tempfile::Builder::new()
            .prefix(&format!("livestream-rs-segment-{}-", stream_id))
            .tempdir_in(&cache_root)?;

        let upload_dir = cache_root.join("livestream-rs-artifacts").join(stream_id);
        fs::create_dir_all(&upload_dir)?;

        Ok(Self {
            segment_dir,
            upload_dir,
        })
    }

    pub fn write_ts_file(&self, data: &[u8], sequence: u64) -> Result<PathBuf> {
        let filename = Self::segment_filename(sequence);
        let path = self.segment_dir.path().join(&filename);
        fs::write(&path, data)?;
        Ok(path)
    }

    pub fn stage_segment(&self, segment_path: &Path) -> Result<PathBuf> {
        let filename = segment_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow::anyhow!("Invalid segment filename"))?;
        let staged_path = self.upload_dir.join(filename);

        if staged_path.exists() {
            fs::remove_file(&staged_path)?;
        }

        match fs::rename(segment_path, &staged_path) {
            Ok(_) => Ok(staged_path),
            Err(_) => {
                fs::copy(segment_path, &staged_path)?;
                fs::remove_file(segment_path)?;
                Ok(staged_path)
            }
        }
    }

    pub fn playlist_path(&self) -> PathBuf {
        self.upload_dir.join("index.m3u8")
    }

    pub fn segment_filename(sequence: u64) -> String {
        format!("segment_{:04}.ts", sequence)
    }
}

// ── PlaylistState ──

use std::collections::VecDeque;

struct PlaylistEntry {
    filename: String,
    duration: f64,
}

pub struct PlaylistState {
    playlist_size: usize,
    entries: VecDeque<PlaylistEntry>,
    media_sequence: u64,
}

impl PlaylistState {
    pub fn new(cfg: &SegmentConfig) -> Self {
        Self {
            playlist_size: cfg.playlist_size.max(1),
            entries: VecDeque::new(),
            media_sequence: 0,
        }
    }

    pub fn push_segment(&mut self, filename: &str, duration: Duration) {
        while self.entries.len() >= self.playlist_size {
            self.entries.pop_front();
            self.media_sequence += 1;
        }
        self.entries.push_back(PlaylistEntry {
            filename: filename.to_string(),
            duration: duration.as_secs_f64(),
        });
    }

    pub fn write_playlist(&self, path: &Path) -> Result<()> {
        let content = self.render(false);
        fs::write(path, content)?;
        Ok(())
    }

    pub fn write_final_playlist(&self, path: &Path) -> Result<()> {
        let content = self.render(true);
        fs::write(path, content)?;
        Ok(())
    }

    fn render(&self, is_final: bool) -> String {
        let max_dur = self
            .entries
            .iter()
            .map(|e| e.duration.ceil() as u64)
            .max()
            .unwrap_or(1);

        let mut out = String::new();
        out.push_str("#EXTM3U\n");
        out.push_str("#EXT-X-VERSION:3\n");
        out.push_str(&format!("#EXT-X-TARGETDURATION:{}\n", max_dur));
        out.push_str(&format!("#EXT-X-MEDIA-SEQUENCE:{}\n", self.media_sequence));

        for entry in &self.entries {
            out.push_str(&format!("#EXTINF:{:.3},\n", entry.duration));
            out.push_str(&format!("{}\n", entry.filename));
        }

        if is_final {
            out.push_str("#EXT-X-ENDLIST\n");
        }

        out
    }
}

// ── HlsMuxerState ──

struct HlsMuxerState {
    hls_ctx: Option<HlsOutputContext>,
    // Box is REQUIRED: parking_lot::Mutex stores T inline. Without Box,
    // Vec<u8> moves when the outer HlsSegmenter is moved (Ok(this) →
    // Arc::new), invalidating the AVIO opaque pointer.
    #[allow(clippy::box_collection)]
    ts_buffer: Box<Vec<u8>>,
    next_sequence: u64,
    /// Last written DTS value (in 90kHz ticks). Used to enforce
    /// monotonicity across muxer context resets.
    last_dts: i64,
}
struct SegmentState {
    first_pts: Option<i64>,
    last_pts: Option<i64>,
}

impl SegmentState {
    fn elapsed(&self) -> Option<Duration> {
        match (self.first_pts, self.last_pts) {
            (Some(first), Some(last)) if last > first => {
                Some(Duration::from_millis((last - first) as u64))
            }
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.first_pts = None;
        self.last_pts = None;
    }
}

// ── HlsSegmenter ──

pub struct HlsSegmenter {
    stream_id: String,
    streams: StaticStreamCollection,
    segment_duration: Duration,
    muxer: Mutex<HlsMuxerState>,
    segmenter: Mutex<SegmentState>,
    playlist: Mutex<PlaylistState>,
    workspace: SegmentWorkspace,
    input: PadReceiver<EncodedPacket>,
    outputs: Vec<PadSender<TsSegment>>,
}

impl HlsSegmenter {
    pub fn new(
        stream_id: &str,
        streams: StaticStreamCollection,
        cfg: &SegmentConfig,
        input: PadReceiver<EncodedPacket>,
        outputs: Vec<PadSender<TsSegment>>,
    ) -> Result<Self> {
        let workspace = SegmentWorkspace::new(stream_id, cfg)?;
        let playlist = PlaylistState::new(cfg);

        // Box<Vec<u8>> ensures the Vec is at a stable heap address even
        // when the outer HlsSegmenter struct is moved (parking_lot::Mutex
        // stores data inline). The opaque pointer is taken BEFORE Ok(this)
        // moves the struct into Arc::new.
        let mut ts_buffer = Box::new(Vec::new());
        let opaque = &mut *ts_buffer as *mut Vec<u8> as *mut std::ffi::c_void;

        let mut muxer_state = HlsMuxerState {
            hls_ctx: None,
            ts_buffer,
            next_sequence: 0,
            last_dts: 0,
        };
        let mut hls_ctx = unsafe { HlsOutputContext::create(&streams, opaque) }?;
        hls_ctx.write_header()?;
        muxer_state.hls_ctx = Some(hls_ctx);

        Ok(Self {
            stream_id: stream_id.into(),
            segment_duration: Duration::from_secs(cfg.duration_secs),
            streams,
            muxer: Mutex::new(muxer_state),
            segmenter: Mutex::new(SegmentState {
                first_pts: None,
                last_pts: None,
            }),
            playlist: Mutex::new(playlist),
            workspace,
            input,
            outputs,
        })
    }

    fn write_packet(&self, pkt: &EncodedPacket) -> Result<()> {
        let mut av_pkt = pkt.to_av_packet()?;
        av_pkt.rescale_ts(
            AVRational { num: 1, den: 1000 },
            AVRational { num: 1, den: 90000 },
        );

        let mut muxer = self.muxer.lock();

        // Enforce monotonically increasing timestamps. The double-rescale
        // (original timebase → ms → 90kHz) loses precision; we track the
        // last DTS and force each new value to be strictly greater.
        // We set both PTS and DTS to the same value so PTS >= DTS always holds.
        let orig_pts = unsafe { (*av_pkt.as_ptr()).pts };
        let next = orig_pts.max(muxer.last_dts + 1);
        unsafe {
            let ptr = av_pkt.as_mut_ptr();
            (*ptr).pts = next;
            (*ptr).dts = next;
        }
        muxer.last_dts = next;

        let ctx = muxer
            .hls_ctx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("HlsOutputContext not initialized"))?;
        ctx.write_frame(&av_pkt)?;
        Ok(())
    }

    fn rollover(&self, duration: Duration) -> Result<TsSegment> {
        // Phase 1: under lock — flush trailer, extract buffer, recreate ctx
        let (seq, ts_data) = {
            let mut muxer = self.muxer.lock();

            if let Some(ref ctx) = muxer.hls_ctx {
                ctx.write_trailer()?;
            }

            let seq = muxer.next_sequence;
            let ts_data = std::mem::take(&mut muxer.ts_buffer);
            muxer.next_sequence += 1;

            // Disarm old context BEFORE creating the new one.
            // Both contexts share the same opaque memory location
            // (muxer.ts_buffer). If the old context's Drop writes
            // a safety-net trailer, it corrupts the new segment buffer.
            if let Some(ref mut ctx) = muxer.hls_ctx {
                ctx.disarm();
            }

            // Re-create TS muxer for the next segment (opaque pointer from Box).
            let new_opaque = &mut *muxer.ts_buffer as *mut Vec<u8> as *mut std::ffi::c_void;
            let mut new_ctx = unsafe { HlsOutputContext::create(&self.streams, new_opaque) }?;
            new_ctx.write_header()?;
            muxer.hls_ctx = Some(new_ctx);
            muxer.last_dts = 0; // new muxer context starts with fresh DTS tracking

            (seq, ts_data)
        }; // lock released here — I/O happens outside

        // Phase 2: disk I/O outside lock
        let ts_path = self.workspace.write_ts_file(&ts_data, seq)?;
        let staged_path = self.workspace.stage_segment(&ts_path)?;

        let filename = SegmentWorkspace::segment_filename(seq);
        Ok(TsSegment {
            path: staged_path,
            filename,
            sequence: seq,
            duration,
            is_final: false,
        })
    }
}

impl Node for HlsSegmenter {
    fn name(&self) -> &str {
        "hls-segmenter"
    }
}

#[async_trait::async_trait]
impl Processor for HlsSegmenter {
    type Input = EncodedPacket;
    type Output = TsSegment;

    fn input_codec(&self) -> &[CodecParams] {
        &[]
    }

    fn output_codec(&self) -> &[CodecParams] {
        &[]
    }

    fn input(&self) -> &PadReceiver<Self::Input> {
        &self.input
    }

    fn outputs(&self) -> &[PadSender<Self::Output>] {
        &self.outputs
    }

    fn should_process(&self) -> bool {
        true // always recording
    }

    async fn process(&self, pkt: Self::Input) -> Result<Vec<Self::Output>> {
        // Sequence headers are synthetic (extradata only, no frame data).
        // Writing them to the TS muxer triggers "h264 bitstream malformed".
        if pkt.is_sequence_header {
            return Ok(vec![]);
        }
        {
            let mut state = self.segmenter.lock();
            if state.first_pts.is_none() {
                state.first_pts = pkt.pts_ms;
            }
            state.last_pts = pkt.pts_ms;
        }

        // Write packet to TS muxer (in-memory buffer)
        match self.write_packet(&pkt) {
            Ok(()) => {}
            Err(e) => {
                metric_pipeline_error!("hls_muxer");
                tracing::warn!(
                    stream = %self.stream_id,
                    error = %e,
                    "TS muxer write failed, resetting segment"
                );
                // Error recovery: drop broken context, clear buffer, recreate.
                // Lock segmenter first to preserve consistent lock ordering.
                self.segmenter.lock().reset();
                let mut muxer = self.muxer.lock();
                // Explicitly drop broken context before clearing buffer
                muxer.hls_ctx = None;
                muxer.ts_buffer.clear();
                muxer.last_dts = 0;
                let new_opaque = &mut *muxer.ts_buffer as *mut Vec<u8> as *mut std::ffi::c_void;
                match unsafe { HlsOutputContext::create(&self.streams, new_opaque) } {
                    Ok(mut new_ctx) => {
                        if let Err(e) = new_ctx.write_header() {
                            tracing::error!(error = %e, "Failed to write header on recreated HLS context");
                            muxer.hls_ctx = None;
                        } else {
                            muxer.hls_ctx = Some(new_ctx);
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to recreate HLS context after write error");
                        muxer.hls_ctx = None;
                    }
                }
                return Ok(vec![]);
            }
        }

        {
            let state = self.segmenter.lock();
            if pkt.is_keyframe && state.elapsed().unwrap_or_default() >= self.segment_duration {
                let duration = state.elapsed().unwrap_or_default();
                drop(state);
                let seg = self.rollover(duration)?;

                // Single lock acquisition for all segmenter state updates
                {
                    let mut s = self.segmenter.lock();
                    s.reset();
                    s.first_pts = pkt.pts_ms;
                    s.last_pts = pkt.pts_ms;
                }

                let mut playlist = self.playlist.lock();
                playlist.push_segment(&seg.filename, seg.duration);
                let _ = playlist.write_playlist(&self.workspace.playlist_path());
                drop(playlist);

                return Ok(vec![seg]);
            }
        }

        Ok(vec![])
    }

    async fn close(&self) -> Result<()> {
        let duration = self.segmenter.lock().elapsed().unwrap_or_default();
        let mut seg = self.rollover(duration)?;
        seg.is_final = true;

        let mut playlist = self.playlist.lock();
        playlist.push_segment(&seg.filename, seg.duration);
        let _ = playlist.write_final_playlist(&self.workspace.playlist_path());
        drop(playlist);

        for pad in &self.outputs {
            let _ = pad.send(seg.clone());
        }
        Ok(())
    }
}
