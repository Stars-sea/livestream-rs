//! PipelineFactory — convenience wiring for standard pipeline construction.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use parking_lot::Mutex;
use tokio::task::JoinHandle;

use crate::broadcast::FlvBroadcast;
use crate::processor::flv_mux::FlvMux;
use crate::processor::hls_segment::HlsSegmenter;
use crate::processor::otel::OTelProbe;
use crate::processor::rtp_depack::RtpDemuxProcessor;
use crate::processor::seq_cache::SeqCacheProbe;
use crate::processor::transcode::TranscodeProcessor;
use crate::sink::flv::FlvSink;
use crate::sink::minio::{MinIoSink, ObjectUploader};
use anyhow::Result;
use livestream_codec::{CodecParams, EncodedPacket, MediaType, RtpPacket};
use livestream_core::config::{SegmentConfig, TranscodeConfig};
use livestream_core::pad::{DemandSignal, PadReceiver, PadSender};
use livestream_core::traits::PipelineHandle;
use livestream_core::types::Codec;
use livestream_media::stream::StaticStreamCollection;
use tokio_util::sync::CancellationToken;

use crate::engine::PipelineImpl;
use crate::task::{run_processor, run_sink};

// ── NullUploader (dev/test fallback) ──

/// No-op ObjectUploader that logs a warning and drops segments.
/// Used when MinIO is not configured (dev/test only).
struct NullUploader;

#[async_trait::async_trait]
impl ObjectUploader for NullUploader {
    async fn upload_file(&self, key: &str, path: &Path) -> Result<()> {
        tracing::warn!(key=%key, path=%path.display(), "NullUploader: dropping segment");
        let _ = std::fs::remove_file(path);
        Ok(())
    }
}

/// Create a no-op ObjectUploader for dev/test when no MinIO is configured.
pub fn null_uploader() -> Arc<dyn ObjectUploader> {
    Arc::new(NullUploader)
}

// ── PipelineFactory ──

/// Holds shared dependencies for constructing pipeline instances.
///
/// Created once at startup and passed to protocol servers (RTMP, RTSP).
/// Each `build_*` call produces an independent `PipelineImpl`.
pub struct PipelineFactory {
    segment_cfg: SegmentConfig,
    minio: Arc<dyn ObjectUploader>,
}

impl PipelineFactory {
    pub fn new(segment_cfg: SegmentConfig, minio: Arc<dyn ObjectUploader>) -> Self {
        Self { segment_cfg, minio }
    }

    /// Access the segment configuration for protocol server construction.
    pub fn segment_cfg(&self) -> &SegmentConfig {
        &self.segment_cfg
    }

    /// Access the object uploader for protocol server construction.
    pub fn minio(&self) -> &Arc<dyn ObjectUploader> {
        &self.minio
    }
}

// ── Public free-function API ──

/// Build a standard pipeline for an EncodedPacket source (RTMP path).
///
/// Creates: OTelProbe → SeqCacheProbe → [FlvMux→FlvSink, HlsSegmenter→MinIoSink].
/// Spawns all processor/sink tasks and returns a `PipelineImpl`.
pub fn build_pipeline(
    live_id: &str,
    src_rx: PadReceiver<EncodedPacket>,
    codec_params: &[CodecParams],
    flv_broadcast: Arc<dyn FlvBroadcast>,
    minio: Arc<dyn ObjectUploader>,
    segment_cfg: &SegmentConfig,
    cancel: CancellationToken,
) -> Result<PipelineImpl> {
    let (handle, futures, deferred_tasks) = build_encoded_chain(
        live_id,
        src_rx,
        codec_params,
        flv_broadcast,
        minio,
        segment_cfg,
        cancel,
    )?;
    let tasks: Vec<_> = futures.into_iter().map(|f| tokio::spawn(f)).collect();
    let pipeline = PipelineImpl::with_shared_tasks(handle, deferred_tasks);
    pipeline.push_tasks(tasks);
    Ok(pipeline)
}

/// Build a pipeline for an RTP source (RTSP path).
///
#[allow(clippy::too_many_arguments)]
/// Chains RtpDemuxProcessor before the standard EncodedPacket pipeline.
/// MJPEG sources get a TranscodeProcessor (MJPEG → H.264) inserted between
/// the demuxer and the encoded chain.
pub fn build_rtsp_pipeline(
    live_id: &str,
    rtp_rx: PadReceiver<RtpPacket>,
    sdp: &str,
    codec_params: &[CodecParams],
    flv_broadcast: Arc<dyn FlvBroadcast>,
    minio: Arc<dyn ObjectUploader>,
    segment_cfg: &SegmentConfig,
    transcode_cfg: &TranscodeConfig,
    cancel: CancellationToken,
) -> Result<PipelineImpl> {
    let (enc_tx, enc_rx) = PadSender::<EncodedPacket>::new_channel(256);
    let depack = Arc::new(RtpDemuxProcessor::new(
        live_id,
        sdp,
        codec_params.to_vec(),
        rtp_rx,
        vec![enc_tx],
    )?);

    let rtp_future: Pin<Box<dyn Future<Output = ()> + Send>> =
        Box::pin(run_processor(depack, cancel.child_token()));

    // MJPEG cannot be muxed to FLV or HLS, so it is transcoded to H.264
    // server-side; all other sources feed the encoded chain directly.
    let has_mjpeg = codec_params.iter().any(|p| p.codec == Codec::Mjpeg);
    let (chain_rx, transcode_future): (PadReceiver<EncodedPacket>, Option<BoxedFuture>) =
        if has_mjpeg {
            let (t_tx, t_rx) = PadSender::<EncodedPacket>::new_channel(256);
            let tp = Arc::new(TranscodeProcessor::new(
                live_id,
                codec_params,
                transcode_cfg.clone(),
                enc_rx,
                vec![t_tx],
            )?);
            (
                t_rx,
                Some(Box::pin(run_processor(tp, cancel.child_token()))),
            )
        } else {
            (enc_rx, None)
        };

    // The transcoder emits its own H.264 sequence header, so the chain must
    // build HLS lazily (deferred_hls_init), the same way RTMP sources do.
    let chain_params: &[CodecParams] = if has_mjpeg { &[] } else { codec_params };

    let (handle, encoded_futures, deferred_tasks) = build_encoded_chain(
        live_id,
        chain_rx,
        chain_params,
        flv_broadcast,
        minio,
        segment_cfg,
        cancel,
    )?;

    let mut all_futures = vec![rtp_future];
    all_futures.extend(transcode_future);
    all_futures.extend(encoded_futures);
    let tasks: Vec<_> = all_futures.into_iter().map(|f| tokio::spawn(f)).collect();
    let pipeline = PipelineImpl::with_shared_tasks(handle, deferred_tasks);
    pipeline.push_tasks(tasks);
    Ok(pipeline)
}

// ── Private helpers ──

type BoxedFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type BuildResult = (
    PipelineHandle,
    Vec<BoxedFuture>,
    Arc<Mutex<Vec<JoinHandle<()>>>>,
);

/// Build the standard EncodedPacket processing chain.
///
/// Always builds the FLV path (OTelProbe → SeqCacheProbe → FlvMux → FlvSink).
/// HLS path (HlsSegmenter → MinIoSink) is added immediately when `codec_params`
/// is non-empty, or deferred via `deferred_hls_init` for RTMP sources.
/// HLS construction failures are logged as warnings and do not affect the FLV path.
pub fn build_encoded_chain(
    live_id: &str,
    src_rx: PadReceiver<EncodedPacket>,
    codec_params: &[CodecParams],
    flv_broadcast: Arc<dyn FlvBroadcast>,
    minio: Arc<dyn ObjectUploader>,
    segment_cfg: &SegmentConfig,
    cancel: CancellationToken,
) -> Result<BuildResult> {
    let has_codec_params = !codec_params.is_empty();
    let deferred_tasks: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));

    // Phase 1: OTelProbe
    let (otel_tx, otel_rx) = PadSender::<EncodedPacket>::new_channel(256);
    let otel = Arc::new(OTelProbe::new(live_id, src_rx, vec![otel_tx]));

    // Phase 2: SeqCacheProbe — fans out to FLV and HLS.
    // Always include both outputs; the HLS channel is used either immediately
    // (when codec_params are available) or by deferred_hls_init (RTMP path).
    let (seq_flv_tx, seq_flv_rx) = PadSender::<EncodedPacket>::new_channel(256);
    let (seq_hls_tx, seq_hls_rx) = PadSender::<EncodedPacket>::new_channel(256);
    let seq_outputs = vec![seq_flv_tx, seq_hls_tx];
    let seq_cache = Arc::new(SeqCacheProbe::new(otel_rx, seq_outputs));

    // Phase 3: FLV path (always)
    let (flv_tx, flv_rx) = PadSender::<livestream_media::flv::FlvTag>::new_channel(256);
    let flv_demand = flv_tx.demand().new_handle();
    let flv_mux = Arc::new(FlvMux::new(live_id, seq_flv_rx, vec![flv_tx]));
    let flv_sink = Arc::new(FlvSink::new(live_id, flv_broadcast, flv_rx, flv_demand));

    let mut futures: Vec<BoxedFuture> = vec![
        Box::pin(run_processor(otel, cancel.child_token())),
        Box::pin(run_processor(seq_cache, cancel.child_token())),
        Box::pin(run_processor(flv_mux, cancel.child_token())),
        Box::pin(run_sink(flv_sink, cancel.child_token())),
    ];

    // Phase 4: HLS path — immediate when codec_params available, deferred otherwise (RTMP)
    if has_codec_params {
        match try_build_hls(
            live_id,
            codec_params,
            minio,
            segment_cfg,
            seq_hls_rx,
            &cancel,
        ) {
            Ok(hls_futures) => futures.extend(hls_futures),
            Err(e) => {
                tracing::warn!(
                    live_id = %live_id,
                    error = %e,
                    "HLS pipeline construction failed, continuing with FLV only"
                );
            }
        }
    } else {
        futures.push(Box::pin(deferred_hls_init(
            live_id.to_string(),
            seq_hls_rx,
            minio.clone(),
            segment_cfg.clone(),
            cancel.child_token(),
            Arc::clone(&deferred_tasks),
        )));
    }

    let handle = PipelineHandle::new(cancel);
    Ok((handle, futures, deferred_tasks))
}

/// Convert AVCDecoderConfigurationRecord extradata to raw SPS+PPS
/// (concatenated, no Annex B start codes) for the MPEG-TS muxer.
/// This prevents FFmpeg from auto-attaching h264_mp4toannexb BSF.
/// Returns the original extradata unchanged for non-H.264 codecs.
fn hls_extradata(codec: Codec, extradata: &[u8]) -> Vec<u8> {
    if codec != Codec::H264 || extradata.len() < 7 {
        return extradata.to_vec();
    }
    let num_sps = (extradata[5] & 0x1F) as usize;
    let mut pos = 6usize;
    let mut out = Vec::new();
    for _ in 0..num_sps {
        if pos + 2 > extradata.len() {
            return extradata.to_vec();
        }
        let len = u16::from_be_bytes([extradata[pos], extradata[pos + 1]]) as usize;
        pos += 2;
        if pos + len > extradata.len() {
            return extradata.to_vec();
        }
        out.extend_from_slice(&extradata[pos..pos + len]);
        pos += len;
    }
    if pos >= extradata.len() {
        return out;
    }
    let num_pps = extradata[pos] as usize;
    pos += 1;
    for _ in 0..num_pps {
        if pos + 2 > extradata.len() {
            return extradata.to_vec();
        }
        let len = u16::from_be_bytes([extradata[pos], extradata[pos + 1]]) as usize;
        pos += 2;
        if pos + len > extradata.len() {
            return extradata.to_vec();
        }
        out.extend_from_slice(&extradata[pos..pos + len]);
        pos += len;
    }
    out
}

///
/// Used for RTMP sources where codec params (SPS/PPS, ASC) arrive in-band
/// after pipeline construction. Collects one CodecParams per codec (video +
/// audio) from sequence-header packets, then constructs the HLS segmenter +
/// MinIO sink chain once both codecs are known, or once media packets start
/// arriving with at least one param collected (audio-only / video-only
/// streams send a single sequence header and must still start).
async fn deferred_hls_init(
    live_id: String,
    hls_rx: PadReceiver<EncodedPacket>,
    minio: Arc<dyn ObjectUploader>,
    segment_cfg: SegmentConfig,
    cancel: CancellationToken,
    deferred_tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
) {
    // At most one param per codec; kept in deterministic order video, audio.
    let mut video_params: Option<CodecParams> = None;
    let mut audio_params: Option<CodecParams> = None;

    loop {
        let pkt = tokio::select! {
            pkt = hls_rx.recv() => pkt,
            _ = cancel.cancelled() => return,
        };
        let Some(pkt) = pkt else { return };

        // Sequence headers carry the codec initialization data (SPS+PPS for
        // H.264 via `extradata`, AudioSpecificConfig for AAC via `data`).
        if pkt.is_sequence_header {
            if record_sequence_header(&pkt, &mut video_params, &mut audio_params) {
                // Both codecs known — build the HLS chain with all params.
                let params: Vec<CodecParams> = video_params
                    .take()
                    .into_iter()
                    .chain(audio_params.take())
                    .collect();
                spawn_deferred_hls(
                    &live_id,
                    params,
                    minio,
                    segment_cfg,
                    hls_rx,
                    cancel,
                    &deferred_tasks,
                );
                return;
            }
            continue;
        }

        // Non-sequence-header media packet: start HLS with whatever params
        // have been collected so far (single-codec streams never send a
        // second sequence header).
        if video_params.is_some() || audio_params.is_some() {
            let params: Vec<CodecParams> = video_params
                .take()
                .into_iter()
                .chain(audio_params.take())
                .collect();
            spawn_deferred_hls(
                &live_id,
                params,
                minio,
                segment_cfg,
                hls_rx,
                cancel,
                &deferred_tasks,
            );
            return;
        }
    }
}

/// Record a sequence-header packet's codec params (at most one per codec).
/// Returns `true` once both a video and an audio param are known.
fn record_sequence_header(
    pkt: &EncodedPacket,
    video_params: &mut Option<CodecParams>,
    audio_params: &mut Option<CodecParams>,
) -> bool {
    let Some((media_type, params)) = codec_params_from_sequence_header(pkt) else {
        return false;
    };
    match media_type {
        MediaType::Video => {
            video_params.get_or_insert(params);
        }
        MediaType::Audio => {
            audio_params.get_or_insert(params);
        }
    }
    video_params.is_some() && audio_params.is_some()
}

/// Build a `CodecParams` from a sequence-header packet, or `None` when the
/// packet carries no codec initialization data.
fn codec_params_from_sequence_header(pkt: &EncodedPacket) -> Option<(MediaType, CodecParams)> {
    let extradata = match (&pkt.extradata, pkt.data.as_bytes()) {
        (Some(ext), _) if !ext.is_empty() => ext.clone(),
        (_, data) if !data.is_empty() => Bytes::copy_from_slice(data),
        _ => return None,
    };
    let media_type = match pkt.codec {
        Codec::H264 | Codec::H265 | Codec::Av1 | Codec::Mjpeg => MediaType::Video,
        Codec::Aac | Codec::Mp3 | Codec::Opus => MediaType::Audio,
    };
    let params = CodecParams {
        codec: pkt.codec,
        media_type,
        clock_rate: 90000u32,
        extradata: Some(Bytes::from(hls_extradata(pkt.codec, &extradata))),
    };
    Some((media_type, params))
}

/// Build and spawn the HLS branch once codec params are available.
fn spawn_deferred_hls(
    live_id: &str,
    params: Vec<CodecParams>,
    minio: Arc<dyn ObjectUploader>,
    segment_cfg: SegmentConfig,
    hls_rx: PadReceiver<EncodedPacket>,
    cancel: CancellationToken,
    deferred_tasks: &Arc<Mutex<Vec<JoinHandle<()>>>>,
) {
    if params.is_empty() {
        return;
    }

    match try_build_hls(live_id, &params, minio, &segment_cfg, hls_rx, &cancel) {
        Ok(hls_futures) => {
            let handles: Vec<_> = hls_futures.into_iter().map(|f| tokio::spawn(f)).collect();
            deferred_tasks.lock().extend(handles);
        }
        Err(e) => {
            tracing::warn!(live_id = %live_id, error = %e, "Deferred HLS build failed");
        }
    }
}

/// Attempt to build the HLS branch (HlsSegmenter → MinIoSink).
/// Returns boxed futures on success, or an error if construction fails.
fn try_build_hls(
    live_id: &str,
    codec_params: &[CodecParams],
    minio: Arc<dyn ObjectUploader>,
    segment_cfg: &SegmentConfig,
    hls_input: PadReceiver<EncodedPacket>,
    cancel: &CancellationToken,
) -> Result<Vec<BoxedFuture>> {
    let streams = StaticStreamCollection::from_codec_params(codec_params)?;
    let (hls_tx, hls_rx) = PadSender::<livestream_codec::TsSegment>::new_channel(256);
    let hls_demand = DemandSignal::new_always_wanted().new_handle();

    let hls_segmenter = Arc::new(HlsSegmenter::new(
        live_id,
        streams,
        segment_cfg,
        hls_input,
        vec![hls_tx],
    )?);

    let minio_sink = Arc::new(MinIoSink::new(
        live_id,
        minio,
        segment_cfg.clone(),
        hls_rx,
        hls_demand,
    ));

    Ok(vec![
        Box::pin(run_processor(hls_segmenter, cancel.child_token())),
        Box::pin(run_sink(minio_sink, cancel.child_token())),
    ])
}
