//! PipelineFactory — convenience wiring for standard pipeline construction.
//!
//! Deferred to Phase 4.1 when Source trait implementations exist and
//! pad wiring is fully implemented.

use std::sync::Arc;

use anyhow::Result;
use livestream_codec::{EncodedPacket, SegmentConfig};
use livestream_core::{traits::Source, types::MediaPacket};
use livestream_media::stream::StaticStreamCollection;
use tokio_util::sync::CancellationToken;

use crate::broadcast::FlvBroadcast;
use crate::sink::minio::ObjectUploader;

/// Build a standard pipeline for a live stream (DEFERRED).
#[allow(unused_variables)]
pub fn build_pipeline(
    live_id: &str,
    source: Arc<dyn Source<Output = EncodedPacket>>,
    flv_broadcast: Arc<dyn FlvBroadcast>,
    minio: Arc<dyn ObjectUploader>,
    segment_cfg: &SegmentConfig,
    cancel: CancellationToken,
) -> Result<crate::engine::PipelineImpl> {
    let _streams = {
        let params = source.codec_params();
        if params.is_empty() {
            anyhow::bail!("Source has no codec parameters");
        }
        // Build StaticStreamCollection from codec params
        let video_tb = livestream_media::ffmpeg_sys_next::AVRational { num: 1, den: 90000 };
        let audio_tb = livestream_media::ffmpeg_sys_next::AVRational { num: 1, den: 44100 };
        let mut owned: Vec<(
            usize,
            livestream_media::ffmpeg_sys_next::AVRational,
            livestream_media::codec::OwnedCodecParams,
        )> = Vec::new();
        for (i, p) in params.iter().enumerate() {
            let tb = if p.is_video() { video_tb } else { audio_tb };
            owned.push((
                i,
                tb,
                livestream_media::codec::OwnedCodecParams::from_codec_params(p)?,
            ));
        }
        StaticStreamCollection::from_owned_params(owned)
    };

    let _ = live_id;
    let _ = flv_broadcast;
    let _ = minio;
    let _ = segment_cfg;
    let _ = cancel;
    let _ = _streams;
    anyhow::bail!("PipelineFactory::build_pipeline deferred to Phase 4.1")
}
