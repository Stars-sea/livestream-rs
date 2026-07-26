mod common;

use std::sync::Arc;
use std::time::Duration;

use common::SpyFlvBroadcast;
use livestream_codec::EncodedPacket;
use livestream_core::config::SegmentConfig;
use livestream_core::pad::PadSender;
use livestream_pipeline::factory::{build_encoded_chain, null_uploader};
use tokio_util::sync::CancellationToken;

/// Feed one video keyframe, verify it reaches FlvSink as an FlvTag.
#[tokio::test]
async fn test_flv_pipeline_one_video_keyframe() {
    let (tx, rx) = PadSender::<EncodedPacket>::new_channel(256);
    let spy = Arc::new(SpyFlvBroadcast::new());
    let uploader = null_uploader();
    let segment_cfg = SegmentConfig::default();
    let cancel = CancellationToken::new();

    // Feed one video keyframe packet.
    tx.send(EncodedPacket::new_video_keyframe(
        [0u8; 100].as_slice(),
        0,
        0,
        0,
    ))
    .unwrap();
    drop(tx); // signal EOF

    let (_handle, futures) = build_encoded_chain(
        "test",
        rx,
        &[],
        spy.clone(),
        uploader,
        &segment_cfg,
        cancel.child_token(),
    )
    .expect("build_encoded_chain should succeed");

    // Run all futures to completion.
    for fut in futures {
        fut.await;
    }

    let tags = spy.tags();
    assert_eq!(tags.len(), 1, "expected exactly 1 FLV tag");
    assert!(
        matches!(tags[0], livestream_media::flv::FlvTag::Video { .. }),
        "expected a Video FLV tag, got: {:?}",
        tags[0]
    );
}

/// Feed sequence headers then media packets in order; verify FlvTags emitted.
#[tokio::test]
async fn test_seq_cache_snapshot_order() {
    let (tx, rx) = PadSender::<EncodedPacket>::new_channel(256);
    let spy = Arc::new(SpyFlvBroadcast::new());
    let uploader = null_uploader();
    let segment_cfg = SegmentConfig::default();
    let cancel = CancellationToken::new();

    // AVC sequence header (video stream 0).
    tx.send(EncodedPacket::new_avc_sequence_header(
        &[0x00, 0x00, 0x00, 0x01],
        &[0x00, 0x00, 0x00, 0x01],
        0,
    ))
    .unwrap();

    // AAC sequence header (audio stream 1).
    tx.send(EncodedPacket::new_aac_sequence_header(&[0x12, 0x10], 1))
        .unwrap();

    // Video keyframe.
    tx.send(EncodedPacket::new_video_keyframe(
        [0u8; 200].as_slice(),
        0,
        0,
        0,
    ))
    .unwrap();

    // Audio packet.
    tx.send(EncodedPacket::new_audio([0u8; 50].as_slice(), 0, 1))
        .unwrap();

    drop(tx); // signal EOF

    let (_handle, futures) = build_encoded_chain(
        "test",
        rx,
        &[],
        spy.clone(),
        uploader,
        &segment_cfg,
        cancel.child_token(),
    )
    .expect("build_encoded_chain should succeed");

    for fut in futures {
        fut.await;
    }

    let tags = spy.tags();
    // SeqCacheProbe forwards cached seq headers + media packets → FlvMux.
    // The exact tag count depends on how FlvMux handles seq headers,
    // but we should have at least the video keyframe and audio packet.
    assert!(
        tags.len() >= 2,
        "expected at least 2 FLV tags (seq headers may be merged), got {}",
        tags.len()
    );

    // Verify tag types are present.
    let has_video = tags
        .iter()
        .any(|t| matches!(t, livestream_media::flv::FlvTag::Video { .. }));
    let has_audio = tags
        .iter()
        .any(|t| matches!(t, livestream_media::flv::FlvTag::Audio { .. }));
    assert!(has_video, "expected at least one Video FLV tag");
    assert!(has_audio, "expected at least one Audio FLV tag");
}

/// When codec_params is empty, HLS branch is skipped.
#[tokio::test]
async fn test_no_hls_when_codec_params_empty() {
    let (tx, rx) = PadSender::<EncodedPacket>::new_channel(256);
    let spy = Arc::new(SpyFlvBroadcast::new());
    let uploader = null_uploader();
    let segment_cfg = SegmentConfig::default();
    let cancel = CancellationToken::new();

    tx.send(EncodedPacket::new_video_keyframe(
        [0u8; 100].as_slice(),
        0,
        0,
        0,
    ))
    .unwrap();
    drop(tx);

    let (_handle, futures) = build_encoded_chain(
        "test",
        rx,
        &[],
        spy.clone(),
        uploader,
        &segment_cfg,
        cancel.child_token(),
    )
    .expect("build_encoded_chain should succeed");

    // FLV path only: OTelProbe, SeqCacheProbe, FlvMux, FlvSink = 4 futures.
    // No HLS branch because codec_params is empty.
    assert_eq!(
        futures.len(),
        4,
        "expected 4 futures (no HLS branch), got {}",
        futures.len()
    );

    for fut in futures {
        fut.await;
    }

    assert!(
        !spy.tags().is_empty(),
        "expected at least one FLV tag even without HLS"
    );
}

/// Pipeline must drain all buffered data when source channel closes.
#[tokio::test]
async fn test_pipeline_shutdown_drains() {
    let (tx, rx) = PadSender::<EncodedPacket>::new_channel(256);
    let spy = Arc::new(SpyFlvBroadcast::new());
    let uploader = null_uploader();
    let segment_cfg = SegmentConfig::default();
    let cancel = CancellationToken::new();

    // Feed one packet then close.
    tx.send(EncodedPacket::new_video_keyframe(
        [0u8; 100].as_slice(),
        0,
        0,
        0,
    ))
    .unwrap();
    drop(tx);

    let (_handle, futures) = build_encoded_chain(
        "test",
        rx,
        &[],
        spy.clone(),
        uploader,
        &segment_cfg,
        cancel.child_token(),
    )
    .expect("build_encoded_chain should succeed");

    let mut joins = Vec::new();
    for fut in futures {
        joins.push(tokio::spawn(fut));
    }

    let all_done = tokio::time::timeout(Duration::from_secs(5), async {
        for join in joins {
            let _ = join.await;
        }
    })
    .await;

    assert!(
        all_done.is_ok(),
        "pipeline futures did not complete within 5 seconds (possible hang)"
    );
    assert!(
        !spy.tags().is_empty(),
        "expected at least 1 FLV tag after pipeline drain, got {}",
        spy.tags().len()
    );
}

/// Full pipeline with HLS branch — verifies HLS path construction.
///
/// With non-empty codec_params, the pipeline should build both FLV and HLS
/// paths (6 futures total). We verify construction succeeds; execution with
/// fake bitstream data is covered by the other FLV-path tests.
#[tokio::test]
async fn test_full_pipeline_with_hls() {
    use livestream_core::types::{Codec, CodecParams};

    let (_tx, rx) = PadSender::<EncodedPacket>::new_channel(256);
    let spy_flv = Arc::new(SpyFlvBroadcast::new());
    let cancel = CancellationToken::new();

    // Valid codec params: H.264 video + AAC audio (with sample_rate set).
    let codec_params = [
        CodecParams::new_video(Codec::H264, 90000, None),
        CodecParams::new_audio(Codec::Aac, 44100, None),
    ];

    let (_handle, futures) = build_encoded_chain(
        "full-pipe-test",
        rx,
        &codec_params,
        spy_flv,
        null_uploader(),
        &SegmentConfig::default(),
        cancel.child_token(),
    )
    .expect("build_encoded_chain with valid codec params should succeed");

    // With non-empty codec_params: OTelProbe + SeqCacheProbe + FlvMux + FlvSink
    // + HlsSegmenter + MinIoSink = 6 futures.
    assert_eq!(
        futures.len(),
        6,
        "expected 6 futures (FLV + HLS paths), got {}",
        futures.len()
    );
}
