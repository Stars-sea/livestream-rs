//! PipelineFactory — convenience wiring for standard pipeline construction.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use livestream_codec::{EncodedPacket, RtpPacket, SegmentConfig};
use livestream_core::pad::{DemandSignal, PadReceiver, PadSender};
use livestream_core::traits::PipelineHandle;
use livestream_core::types::CodecParams;
use livestream_media::ffmpeg_sys_next::AVRational;
use livestream_media::stream::StaticStreamCollection;
use tokio_util::sync::CancellationToken;

use crate::broadcast::FlvBroadcast;
use crate::engine::PipelineImpl;
use crate::processor::{FlvMux, HlsSegmenter, OTelProbe, RtpDemuxProcessor, SeqCacheProbe};
use crate::sink::minio::ObjectUploader;
use crate::sink::{FlvSink, MinIoSink};
use crate::task::{run_processor, run_sink};

// ── NullUploader (dev/test fallback) ──

/// No-op ObjectUploader that logs a warning and drops segments.
/// Used when MinIO is not configured (dev/test only).
struct NullUploader;

#[async_trait::async_trait]
impl ObjectUploader for NullUploader {
    async fn upload_file(&self, _key: &str, _path: &Path) -> Result<()> {
        tracing::warn!("HLS upload skipped: no MinIO configured");
        Ok(())
    }
}

/// Create a no-op ObjectUploader for dev/test when no MinIO is configured.
pub fn null_uploader() -> Arc<dyn ObjectUploader> {
    Arc::new(NullUploader)
}

// ── Public API ──

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
    let (handle, tasks) = build_encoded_chain(
        live_id,
        src_rx,
        codec_params,
        flv_broadcast,
        minio,
        segment_cfg,
        cancel,
    )?;
    Ok(PipelineImpl::new(handle, tasks))
}

/// Build a pipeline for an RTP source (RTSP path).
///
/// Chains RtpDemuxProcessor before the standard EncodedPacket pipeline.
#[allow(clippy::too_many_arguments)]
pub fn build_rtsp_pipeline(
    live_id: &str,
    rtp_rx: PadReceiver<RtpPacket>,
    sdp: &str,
    codec_params: &[CodecParams],
    flv_broadcast: Arc<dyn FlvBroadcast>,
    minio: Arc<dyn ObjectUploader>,
    segment_cfg: &SegmentConfig,
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

    let mut rtp_tasks = vec![tokio::spawn(run_processor(depack, cancel.child_token()))];

    let (handle, encoded_tasks) = build_encoded_chain(
        live_id,
        enc_rx,
        codec_params,
        flv_broadcast,
        minio,
        segment_cfg,
        cancel,
    )?;
    rtp_tasks.extend(encoded_tasks);
    Ok(PipelineImpl::new(handle, rtp_tasks))
}

// ── Private helpers ──

/// Build the standard EncodedPacket processing chain.
///
/// Always builds the FLV path (OTelProbe → SeqCacheProbe → FlvMux → FlvSink).
/// HLS path (HlsSegmenter → MinIoSink) is only added when `codec_params` is non-empty.
/// HLS construction failures are logged as warnings and do not affect the FLV path.
fn build_encoded_chain(
    live_id: &str,
    src_rx: PadReceiver<EncodedPacket>,
    codec_params: &[CodecParams],
    flv_broadcast: Arc<dyn FlvBroadcast>,
    minio: Arc<dyn ObjectUploader>,
    segment_cfg: &SegmentConfig,
    cancel: CancellationToken,
) -> Result<(PipelineHandle, Vec<tokio::task::JoinHandle<()>>)> {
    let has_codec_params = !codec_params.is_empty();

    // Phase 1: OTelProbe
    let (otel_tx, otel_rx) = PadSender::<EncodedPacket>::new_channel(256);
    let otel = Arc::new(OTelProbe::new(live_id, src_rx, vec![otel_tx]));

    // Phase 2: SeqCacheProbe — fans out to FLV and optionally HLS.
    let (seq_flv_tx, seq_flv_rx) = PadSender::<EncodedPacket>::new_channel(256);
    let (seq_hls_tx, seq_hls_rx) = PadSender::<EncodedPacket>::new_channel(256);
    let seq_outputs: Vec<PadSender<EncodedPacket>> = if has_codec_params {
        vec![seq_flv_tx, seq_hls_tx]
    } else {
        vec![seq_flv_tx]
    };
    let seq_cache = Arc::new(SeqCacheProbe::new(otel_rx, seq_outputs));

    // Phase 3: FLV path (always)
    let (flv_tx, flv_rx) = PadSender::<livestream_media::flv::FlvTag>::new_channel(256);
    let flv_demand = flv_tx.demand().new_handle();
    let flv_mux = Arc::new(FlvMux::new(live_id, seq_flv_rx, vec![flv_tx]));
    let flv_sink = Arc::new(FlvSink::new(live_id, flv_broadcast, flv_rx, flv_demand));

    // Spawn FLV tasks
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = vec![
        tokio::spawn(run_processor(otel, cancel.child_token())),
        tokio::spawn(run_processor(seq_cache, cancel.child_token())),
        tokio::spawn(run_processor(flv_mux, cancel.child_token())),
        tokio::spawn(run_sink(flv_sink, cancel.child_token())),
    ];

    // Phase 4: HLS path (only when codec params available)
    if has_codec_params {
        match try_build_hls(
            live_id,
            codec_params,
            minio,
            segment_cfg,
            seq_hls_rx,
            &cancel,
        ) {
            Ok(hls_tasks) => tasks.extend(hls_tasks),
            Err(e) => {
                tracing::warn!(
                    live_id = %live_id,
                    error = %e,
                    "HLS pipeline construction failed, continuing with FLV only"
                );
            }
        }
    } else {
        tracing::info!(
            live_id = %live_id,
            "Skipping HLS pipeline: no codec parameters available (RTMP metadata may arrive later)"
        );
    }

    let handle = PipelineHandle::new(cancel);
    Ok((handle, tasks))
}

/// Attempt to build the HLS branch (HlsSegmenter → MinIoSink).
/// Returns task handles on success, or an error if construction fails.
fn try_build_hls(
    live_id: &str,
    codec_params: &[CodecParams],
    minio: Arc<dyn ObjectUploader>,
    segment_cfg: &SegmentConfig,
    hls_input: PadReceiver<EncodedPacket>,
    cancel: &CancellationToken,
) -> Result<Vec<tokio::task::JoinHandle<()>>> {
    let streams = build_static_streams(codec_params)?;
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
        tokio::spawn(run_processor(hls_segmenter, cancel.child_token())),
        tokio::spawn(run_sink(minio_sink, cancel.child_token())),
    ])
}

/// Build a StaticStreamCollection from codec parameters.
fn build_static_streams(codec_params: &[CodecParams]) -> Result<StaticStreamCollection> {
    let video_tb = AVRational { num: 1, den: 90000 };
    let audio_tb = AVRational { num: 1, den: 44100 };

    let owned: Vec<_> = codec_params
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let tb = if p.is_video() { video_tb } else { audio_tb };
            Ok((
                i,
                tb,
                livestream_media::codec::OwnedCodecParams::from_codec_params(p)?,
            ))
        })
        .collect::<Result<_>>()?;

    Ok(StaticStreamCollection::from_owned_params(owned))
}
