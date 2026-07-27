//! PipelineFactory — convenience wiring for standard pipeline construction.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use crate::broadcast::FlvBroadcast;
use crate::processor::flv_mux::FlvMux;
use crate::processor::hls_segment::HlsSegmenter;
use crate::processor::otel::OTelProbe;
use crate::processor::rtp_depack::RtpDemuxProcessor;
use crate::processor::seq_cache::SeqCacheProbe;
use crate::sink::flv::FlvSink;
use crate::sink::minio::{MinIoSink, ObjectUploader};
use anyhow::Result;
use livestream_codec::{CodecParams, EncodedPacket, MediaType, RtpPacket};
use livestream_core::config::SegmentConfig;
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
    #[allow(dead_code)]
    flv_broadcast: Arc<dyn FlvBroadcast>,
    segment_cfg: SegmentConfig,
    minio: Arc<dyn ObjectUploader>,
}

impl PipelineFactory {
    pub fn new(
        segment_cfg: SegmentConfig,
        minio: Arc<dyn ObjectUploader>,
        flv_broadcast: Arc<dyn FlvBroadcast>,
    ) -> Self {
        Self {
            segment_cfg,
            minio,
            flv_broadcast,
        }
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
    let (handle, futures) = build_encoded_chain(
        live_id,
        src_rx,
        codec_params,
        flv_broadcast,
        minio,
        segment_cfg,
        cancel,
    )?;
    let tasks: Vec<_> = futures.into_iter().map(|f| tokio::spawn(f)).collect();
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

    let rtp_future: Pin<Box<dyn Future<Output = ()> + Send>> =
        Box::pin(run_processor(depack, cancel.child_token()));

    let (handle, encoded_futures) = build_encoded_chain(
        live_id,
        enc_rx,
        codec_params,
        flv_broadcast,
        minio,
        segment_cfg,
        cancel,
    )?;

    let mut all_futures = vec![rtp_future];
    all_futures.extend(encoded_futures);
    let tasks: Vec<_> = all_futures.into_iter().map(|f| tokio::spawn(f)).collect();
    Ok(PipelineImpl::new(handle, tasks))
}

// ── Private helpers ──

type BoxedFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

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
) -> Result<(PipelineHandle, Vec<BoxedFuture>)> {
    let has_codec_params = !codec_params.is_empty();

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
        )));
    }

    let handle = PipelineHandle::new(cancel);
    Ok((handle, futures))
}

/// Wait for the first sequence header (with extradata), then build HLS pipeline lazily.
///
/// Used for RTMP sources where codec params (SPS/PPS) arrive in-band after
/// pipeline construction. Waits on `hls_rx` for a packet carrying extradata,
/// then constructs the HLS segmenter + MinIO sink chain.
#[allow(clippy::excessive_nesting)]
async fn deferred_hls_init(
    live_id: String,
    hls_rx: PadReceiver<EncodedPacket>,
    minio: Arc<dyn ObjectUploader>,
    segment_cfg: SegmentConfig,
    cancel: CancellationToken,
) {
    loop {
        let pkt = tokio::select! {
            pkt = hls_rx.recv() => pkt,
            _ = cancel.cancelled() => return,
        };
        let Some(pkt) = pkt else { return };
        // Wait for the first sequence header (carries extradata = SPS+PPS)
        if let Some(ref extradata) = pkt.extradata {
            let media_type = match pkt.codec {
                Codec::H264 | Codec::H265 | Codec::Av1 => MediaType::Video,
                Codec::Aac | Codec::Mp3 | Codec::Opus => MediaType::Audio,
            };
            let params = vec![CodecParams {
                codec: pkt.codec,
                media_type,
                clock_rate: 90000u32,
                extradata: Some(extradata.clone()),
            }];

            match try_build_hls(&live_id, &params, minio, &segment_cfg, hls_rx, &cancel) {
                Ok(hls_futures) => {
                    for fut in hls_futures {
                        tokio::spawn(fut);
                    }
                    return;
                }
                Err(e) => {
                    tracing::warn!(live_id = %live_id, error = %e, "Deferred HLS build failed");
                    return;
                }
            }
        }
        // Non-header packet — continue waiting
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
