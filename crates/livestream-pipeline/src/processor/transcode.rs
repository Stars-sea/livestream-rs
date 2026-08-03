//! TranscodeProcessor — server-side MJPEG → H.264 transcoding.
//!
//! RTSP sources that announce MJPEG (RFC 2435) cannot be carried by the FLV
//! or HLS muxers (ffmpeg rejects MJPEG in both), so the server decodes each
//! MJPEG frame and re-encodes it as H.264 before packets reach the standard
//! encoded chain. All other codecs (AAC, …) pass through untouched.
//!
//! One instance per stream; all FFmpeg state (decoder, encoder, scaler,
//! frames) lives behind a single `Mutex` and is accessed serially.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use bytes::Bytes;
use livestream_codec::{EncodedPacket, NalData};
use livestream_core::config::TranscodeConfig;
use livestream_core::{
    pad::{PadReceiver, PadSender},
    traits::{Node, Processor},
    types::{Codec, CodecParams, MediaType},
};
use livestream_media::codec::OwnedCodecParams;
use livestream_media::convert::IntoAvPacket;
use livestream_media::decoder::Decoder;
use livestream_media::encoder::Encoder;
use livestream_media::ffmpeg_sys_next::*;
use livestream_media::frame::Frame;
use livestream_media::packet::Packet;
use livestream_media::scaler::Scaler;
use livestream_telemetry::metric_pipeline_error;
use parking_lot::Mutex;

/// All FFmpeg transcode state, serialized behind one mutex per stream.
struct TranscodeState {
    decoder: Option<Decoder>,
    encoder: Option<Encoder>,
    scaler: Option<Scaler>,
    dst_frame: Option<Frame>,
    /// PTS fallback — frames without a PTS advance by 33ms.
    last_pts: i64,
    /// Frames encoded since the encoder was initialized (first frame is
    /// always kept by the fps filter).
    frames_since_init: u64,
    /// Last kept frame's `floor(pts_ms / interval)` bucket for fps downsampling.
    last_kept_slot: i64,
}

/// Decodes MJPEG packets and re-encodes them as H.264 (Annex B, avcC
/// sequence header synthesized from the first keyframe).
pub struct TranscodeProcessor {
    stream_id: String,
    cfg: TranscodeConfig,
    /// MJPEG stream params — kept to re-open the decoder after fatal errors.
    mjpeg_params: CodecParams,
    state: Mutex<TranscodeState>,
    emitted_seq_header: AtomicBool,
    /// Output codec params, recorded once the H.264 sequence header is
    /// emitted. `OnceLock` because `output_codec()` must return a
    /// `&[CodecParams]` borrowed from `&self` (a `Mutex` guard would be a
    /// temporary).
    output_params: OnceLock<Vec<CodecParams>>,
    input: PadReceiver<EncodedPacket>,
    outputs: Vec<PadSender<EncodedPacket>>,
}

impl TranscodeProcessor {
    pub fn new(
        stream_id: &str,
        input_codec_params: &[CodecParams],
        cfg: TranscodeConfig,
        input: PadReceiver<EncodedPacket>,
        outputs: Vec<PadSender<EncodedPacket>>,
    ) -> Result<Self> {
        let mjpeg_params = input_codec_params
            .iter()
            .find(|p| p.codec == Codec::Mjpeg)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("TranscodeProcessor requires a MJPEG video stream"))?;

        // Open the decoder eagerly: configuration problems surface at
        // pipeline build time (ANNOUNCE → RECORD fails, visible to the
        // pusher) instead of mid-stream.
        let mut decoder = Decoder::new(AVCodecID::AV_CODEC_ID_MJPEG)?;
        decoder.open(&OwnedCodecParams::from_codec_params(&mjpeg_params)?)?;

        Ok(Self {
            stream_id: stream_id.to_string(),
            cfg,
            mjpeg_params,
            state: Mutex::new(TranscodeState {
                decoder: Some(decoder),
                encoder: None,
                scaler: None,
                dst_frame: None,
                last_pts: 0,
                frames_since_init: 0,
                last_kept_slot: i64::MIN,
            }),
            emitted_seq_header: AtomicBool::new(false),
            output_params: OnceLock::new(),
            input,
            outputs,
        })
    }

    /// Re-create the MJPEG decoder after a fatal decode error. On failure the
    /// decoder stays `None` and the next packet attempts a rebuild.
    fn rebuild_decoder(&self, state: &mut TranscodeState) {
        let rebuilt = (|| -> Result<Decoder> {
            let mut decoder = Decoder::new(AVCodecID::AV_CODEC_ID_MJPEG)?;
            decoder.open(&OwnedCodecParams::from_codec_params(&self.mjpeg_params)?)?;
            Ok(decoder)
        })();
        match rebuilt {
            Ok(decoder) => {
                state.decoder = Some(decoder);
            }
            Err(e) => {
                state.decoder = None;
                tracing::warn!(stream = %self.stream_id, error = %e, "failed to rebuild MJPEG decoder");
            }
        }
    }

    /// Send one MJPEG packet to the decoder and collect the decoded frames.
    ///
    /// Fatal decode errors are counted, logged, and recovered from (the
    /// decoder is rebuilt); the offending frame is dropped without
    /// interrupting the stream.
    fn decode_frames(&self, state: &mut TranscodeState, pkt: &EncodedPacket) -> Result<Vec<Frame>> {
        if state.decoder.is_none() {
            self.rebuild_decoder(state);
        }
        let Some(decoder) = state.decoder.as_mut() else {
            tracing::warn!(stream = %self.stream_id, "MJPEG decoder unavailable; dropping frame");
            return Ok(vec![]);
        };
        let av_pkt = pkt.to_av_packet()?;
        if let Err(e) = decoder.send_packet(&av_pkt) {
            metric_pipeline_error!("transcode.drop_frame");
            tracing::warn!(stream = %self.stream_id, error = %e, "MJPEG decode send failed; dropping frame");
            self.rebuild_decoder(state);
            return Ok(vec![]);
        }
        let mut frames = Vec::new();
        loop {
            let mut frame = Frame::new()?;
            let Some(decoder) = state.decoder.as_mut() else {
                break;
            };
            match decoder.receive_frame(&mut frame) {
                Ok(true) => frames.push(frame),
                Ok(false) => break,
                Err(e) => {
                    metric_pipeline_error!("transcode.drop_frame");
                    tracing::warn!(stream = %self.stream_id, error = %e, "MJPEG decode error; dropping frame");
                    self.rebuild_decoder(state);
                    break;
                }
            }
        }
        Ok(frames)
    }

    /// Normalize PTS, apply the fps filter, and encode one decoded frame.
    ///
    /// Per-frame failures drop the frame without interrupting the stream.
    fn process_frame(
        &self,
        state: &mut TranscodeState,
        mut frame: Frame,
        out: &mut Vec<EncodedPacket>,
    ) {
        let pts = match frame.pts() {
            Some(p) => p,
            None => {
                let p = state.last_pts + 33;
                state.last_pts = p;
                p
            }
        };
        state.last_pts = pts;
        frame.set_pts(pts);

        if !self.should_keep_frame(state, pts) {
            return;
        }

        if let Err(e) = self.encode_frame(state, &frame, out) {
            metric_pipeline_error!("transcode.drop_frame");
            tracing::warn!(stream = %self.stream_id, error = %e, "transcode encode failed; dropping frame");
        } else {
            state.frames_since_init += 1;
        }
    }

    /// FPS downsampling (only when `cfg.fps` is set): keep the first frame,
    /// then only frames that advance to a new time bucket. Only down-samples —
    /// a source below the target rate keeps every frame.
    fn should_keep_frame(&self, state: &mut TranscodeState, pts: i64) -> bool {
        let Some(fps) = self.cfg.fps else {
            return true;
        };
        let interval_ms = (1000.0 / fps) as i64;
        let slot = pts / interval_ms;
        if state.frames_since_init > 0 {
            if slot <= state.last_kept_slot {
                tracing::debug!(
                    stream = %self.stream_id,
                    pts_ms = pts,
                    "transcode frame dropped by fps filter"
                );
                return false;
            }
            if slot > state.last_kept_slot + 1 {
                tracing::debug!(
                    stream = %self.stream_id,
                    target_fps = fps,
                    "source frame rate appears below target; keeping all frames"
                );
            }
        }
        state.last_kept_slot = slot;
        true
    }

    /// Encode one decoded frame into H.264 packets. The encoder is created
    /// lazily on the first frame (dimensions are only known after decode);
    /// per-frame failures drop the frame without interrupting the stream.
    fn encode_frame(
        &self,
        state: &mut TranscodeState,
        frame: &Frame,
        out: &mut Vec<EncodedPacket>,
    ) -> Result<()> {
        let w = frame.width();
        let h = frame.height();
        if w <= 0 || h <= 0 {
            anyhow::bail!("invalid decoded frame dimensions {}x{}", w, h);
        }
        // MJPEG can produce odd dimensions; x264 requires even ones.
        let ew = w & !1;
        let eh = h & !1;

        if state.encoder.is_none() {
            let bit_rate = (self.cfg.bitrate_kbps * 1000) as i64;
            let time_base = AVRational { num: 1, den: 1000 };
            // GOP target; approximate when the source fps is unknown.
            let fps = self.cfg.fps.unwrap_or(15.0);
            let gop_frames = (self.cfg.gop_secs * fps).round().max(1.0) as i32;
            // preset/tune are read by libx264 at open time, so they must be
            // applied before construction returns (post-open setters are
            // no-ops for libx264). tune=zerolatency disables lookahead so
            // x264 emits each frame immediately instead of buffering.
            let encoder = Encoder::new_named_with_opts(
                "libx264",
                ew,
                eh,
                AVPixelFormat::AV_PIX_FMT_YUV420P,
                time_base,
                bit_rate,
                gop_frames,
                &[("preset", &self.cfg.preset), ("tune", "zerolatency")],
            )
            .or_else(|_| {
                Encoder::new(
                    AVCodecID::AV_CODEC_ID_H264,
                    ew,
                    eh,
                    AVPixelFormat::AV_PIX_FMT_YUV420P,
                    time_base,
                    bit_rate,
                )
            })?;
            state.encoder = Some(encoder);
        }
        let encoder = state.encoder.as_mut().expect("encoder just initialized");

        let src_fmt = frame.format();
        let direct = src_fmt == AVPixelFormat::AV_PIX_FMT_YUV420P as i32 && w == ew && h == eh;
        let to_encode: &Frame = if direct {
            frame
        } else {
            if state.scaler.is_none() {
                // SAFETY: AVPixelFormat is `#[repr(i32)]`; frame.format() is a
                // value FFmpeg produced, so the transmute is valid.
                let src_fmt_enum: AVPixelFormat =
                    unsafe { std::mem::transmute::<i32, AVPixelFormat>(src_fmt) };
                state.scaler = Some(Scaler::new(
                    w,
                    h,
                    src_fmt_enum,
                    ew,
                    eh,
                    AVPixelFormat::AV_PIX_FMT_YUV420P,
                    SwsFlags::SWS_BILINEAR as i32,
                )?);
            }
            if state.dst_frame.is_none() {
                state.dst_frame = Some(Frame::new_with_format(
                    AVPixelFormat::AV_PIX_FMT_YUV420P as i32,
                    ew,
                    eh,
                )?);
            }
            let scaler = state.scaler.as_mut().expect("scaler just initialized");
            let dst = state.dst_frame.as_mut().expect("dst frame just allocated");
            scaler.scale(frame, dst)?;
            // sws_scale does not copy PTS to the destination frame.
            dst.set_pts(frame.pts().unwrap_or(state.last_pts));
            dst
        };

        encoder.send_frame(Some(to_encode))?;
        loop {
            let mut av_pkt = Packet::alloc()?;
            if !encoder.receive_packet(&mut av_pkt)? {
                break;
            }
            let pts_ms = av_pkt.pts();
            out.push(EncodedPacket {
                codec: Codec::H264,
                stream_index: 0,
                data: NalData::AnnexB(Bytes::copy_from_slice(av_pkt.data())),
                pts_ms,
                dts_ms: av_pkt.dts().or(pts_ms),
                is_keyframe: av_pkt.is_key_frame(),
                is_sequence_header: false,
                is_script_data: false,
                extradata: None,
            });
        }
        Ok(())
    }

    /// Emit a synthesized H.264 sequence header (avcC) in front of the first
    /// keyframe's output, and record the output codec params. Skips (with a
    /// warning) when the keyframe lacks SPS/PPS.
    fn maybe_emit_sequence_header(&self, out: &mut Vec<EncodedPacket>) {
        if self.emitted_seq_header.load(Ordering::Acquire) {
            return;
        }
        let Some(keyframe) = out.iter().find(|p| p.is_keyframe && !p.is_sequence_header) else {
            return;
        };
        match extract_sps_pps(keyframe.data.as_bytes()) {
            Some((sps, pps)) => {
                // AVCDecoderConfigurationRecord: version, SPS profile/compat/
                // level, lengthSizeMinusOne=3, 1 SPS, 1 PPS.
                let mut avcc = Vec::with_capacity(11 + sps.len() + pps.len());
                avcc.push(0x01);
                avcc.extend_from_slice(&sps[1..4]);
                avcc.push(0xFF);
                avcc.push(0xE1); // 0xE0 | numSPS(1)
                avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
                avcc.extend_from_slice(&sps);
                avcc.push(0x01); // numPPS
                avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
                avcc.extend_from_slice(&pps);

                let mut sps_pps = Vec::with_capacity(sps.len() + pps.len());
                sps_pps.extend_from_slice(&sps);
                sps_pps.extend_from_slice(&pps);

                out.insert(
                    0,
                    EncodedPacket {
                        codec: Codec::H264,
                        stream_index: 0,
                        data: NalData::AnnexB(Bytes::new()),
                        pts_ms: None,
                        dts_ms: None,
                        is_keyframe: true,
                        is_sequence_header: true,
                        is_script_data: false,
                        extradata: Some(Bytes::from(avcc)),
                    },
                );
                let _ = self.output_params.set(vec![CodecParams {
                    codec: Codec::H264,
                    media_type: MediaType::Video,
                    clock_rate: 90000,
                    extradata: Some(Bytes::from(sps_pps)),
                }]);
                tracing::info!(
                    stream = %self.stream_id,
                    "TranscodeProcessor emitted H.264 sequence header"
                );
            }
            None => {
                tracing::warn!(
                    stream = %self.stream_id,
                    "transcode keyframe lacks SPS/PPS; skipping sequence header \
                     (late FLV subscribers and deferred HLS will not initialize)"
                );
            }
        }
        self.emitted_seq_header.store(true, Ordering::Release);
    }
}

impl Node for TranscodeProcessor {
    fn name(&self) -> &str {
        "transcode"
    }
}

#[async_trait::async_trait]
impl Processor for TranscodeProcessor {
    type Input = EncodedPacket;
    type Output = EncodedPacket;

    fn input_codec(&self) -> &[CodecParams] {
        &[]
    }

    fn output_codec(&self) -> &[CodecParams] {
        self.output_params.get().map(Vec::as_slice).unwrap_or(&[])
    }

    fn input(&self) -> &PadReceiver<Self::Input> {
        &self.input
    }

    fn outputs(&self) -> &[PadSender<Self::Output>] {
        &self.outputs
    }

    /// Must always run — downstream demand flows through the encoded chain,
    /// which no subscriber attaches a demand handle to (same reasoning as
    /// RtpDemuxProcessor).
    fn should_process(&self) -> bool {
        true
    }

    async fn process(&self, pkt: Self::Input) -> Result<Vec<Self::Output>> {
        // Non-MJPEG packets (AAC, …) pass through untouched.
        if pkt.codec != Codec::Mjpeg {
            return Ok(vec![pkt]);
        }
        if pkt.is_sequence_header {
            // MJPEG has no sequence headers — defensive.
            tracing::warn!(
                stream = %self.stream_id,
                "TranscodeProcessor dropping unexpected MJPEG sequence header"
            );
            return Ok(vec![]);
        }

        let mut state = self.state.lock();
        let mut out: Vec<EncodedPacket> = Vec::new();

        // ── Decode → fps filter → encode ──
        for frame in self.decode_frames(&mut state, &pkt)? {
            self.process_frame(&mut state, frame, &mut out);
        }

        // ── Sequence header emission ──
        self.maybe_emit_sequence_header(&mut out);

        Ok(out)
    }
}

/// Return the first SPS (NAL type 7) and PPS (NAL type 8) payloads from an
/// Annex B bitstream (start codes excluded). `None` when either is missing.
fn extract_sps_pps(annexb: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut sps: Option<Vec<u8>> = None;
    let mut pps: Option<Vec<u8>> = None;
    for nal in split_annexb(annexb) {
        if nal.is_empty() {
            continue;
        }
        match nal[0] & 0x1F {
            7 if sps.is_none() => sps = Some(nal.to_vec()),
            8 if pps.is_none() => pps = Some(nal.to_vec()),
            _ => {}
        }
        if sps.is_some() && pps.is_some() {
            break;
        }
    }
    match (sps, pps) {
        (Some(sps), Some(pps)) if sps.len() >= 4 => Some((sps, pps)),
        _ => None,
    }
}

/// Split an Annex B bitstream into NAL units (skipping `00 00 01` and
/// `00 00 00 01` start codes).
fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut start: Option<usize> = None;
    let mut i = 0;
    while i + 2 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(s) = start {
                nals.push(&data[s..i]);
            }
            i += 3;
            start = Some(i);
            continue;
        }
        if i + 3 < data.len()
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1
        {
            if let Some(s) = start {
                nals.push(&data[s..i]);
            }
            i += 4;
            start = Some(i);
            continue;
        }
        i += 1;
    }
    if let Some(s) = start {
        nals.push(&data[s..]);
    }
    nals
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Minimal baseline JPEG (64x48) — decodable by ffmpeg's MJPEG decoder.
    /// Baseline JPEG frame (testsrc2 320x240, `-c:v mjpeg -q:v 5`) kept in
    /// this crate's `tests/data/` (tracked — `testdata/` at the repo root is
    /// gitignored); regenerate with:
    /// `ffmpeg -y -f lavfi -i testsrc2=size=320x240:rate=10 -frames:v 1 -c:v mjpeg -q:v 5 crates/livestream-pipeline/tests/data/mjpeg-frame.jpg`
    const MJPEG_FRAME: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/mjpeg-frame.jpg"
    ));

    fn make_processor() -> (Arc<TranscodeProcessor>, PadSender<EncodedPacket>) {
        let (tx, rx) = PadSender::<EncodedPacket>::new_channel(8);
        let (out_tx, _out_rx) = PadSender::<EncodedPacket>::new_channel(8);
        let proc = TranscodeProcessor::new(
            "test-live",
            &[CodecParams::new_video(Codec::Mjpeg, 90000, None)],
            TranscodeConfig::default(),
            rx,
            vec![out_tx],
        )
        .expect("processor should construct");
        (Arc::new(proc), tx)
    }

    fn mjpeg_packet(pts_ms: i64) -> EncodedPacket {
        EncodedPacket {
            codec: Codec::Mjpeg,
            stream_index: 0,
            data: NalData::AnnexB(Bytes::copy_from_slice(MJPEG_FRAME)),
            pts_ms: Some(pts_ms),
            dts_ms: Some(pts_ms),
            is_keyframe: true,
            is_sequence_header: false,
            is_script_data: false,
            extradata: None,
        }
    }

    #[tokio::test]
    async fn passthrough_non_mjpeg() {
        let (proc, _tx) = make_processor();
        let pkt = EncodedPacket::new_audio(Bytes::copy_from_slice(&[0xFF, 0xF1]), 0, 1);
        let out = proc.process(pkt.clone()).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].codec, Codec::Aac);
        assert_eq!(out[0].data.as_bytes(), pkt.data.as_bytes());
    }

    #[tokio::test]
    async fn transcode_mjpeg_to_h264() {
        let (proc, _tx) = make_processor();
        let out = proc.process(mjpeg_packet(0)).await.unwrap();

        // First output must be the synthesized avcC sequence header.
        assert!(!out.is_empty(), "transcode should produce output");
        let header = &out[0];
        assert!(header.is_sequence_header);
        assert_eq!(header.codec, Codec::H264);
        let ext = header.extradata.as_ref().expect("seq header extradata");
        assert_eq!(ext[0], 0x01, "avcC configurationVersion");
        assert_eq!(ext[5] & 0x1F, 1, "avcC numSPS");

        // Remaining outputs are H.264 keyframes in Annex B.
        for pkt in &out[1..] {
            assert_eq!(pkt.codec, Codec::H264);
            assert!(pkt.is_keyframe);
            assert!(pkt.data.as_bytes().starts_with(&[0x00, 0x00, 0x00, 0x01]));
        }

        // Second frame at pts=100 → encoded pts >= 100.
        let out2 = proc.process(mjpeg_packet(100)).await.unwrap();
        assert!(!out2.is_empty());
        out2.iter()
            .filter_map(|p| p.pts_ms)
            .for_each(|pts| assert!(pts >= 100, "encoded pts {} should follow input", pts));
    }
}
