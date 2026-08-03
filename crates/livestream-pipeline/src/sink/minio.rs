//! MinIoSink — uploads TS segments to MinIO/S3-compatible storage.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use livestream_codec::{SegmentConfig, TsSegment};
use livestream_core::{
    pad::{DemandHandle, PadReceiver},
    traits::{Node, Sink},
    types::{CodecParams, Protocol},
};
use livestream_telemetry::{metric_minio_upload_latency_ms, metric_minio_upload_total};

#[async_trait::async_trait]
pub trait ObjectUploader: Send + Sync {
    async fn upload_file(&self, object_key: &str, file_path: &Path) -> Result<()>;
}

pub struct MinIoSink {
    live_id: String,
    client: Arc<dyn ObjectUploader>,
    segment_cfg: SegmentConfig,
    input: PadReceiver<TsSegment>,
    always_wanted: DemandHandle,
}

impl MinIoSink {
    pub fn new(
        live_id: &str,
        client: Arc<dyn ObjectUploader>,
        segment_cfg: SegmentConfig,
        input: PadReceiver<TsSegment>,
        demand_handle: DemandHandle,
    ) -> Self {
        Self {
            live_id: live_id.into(),
            client,
            segment_cfg,
            input,
            always_wanted: demand_handle,
        }
    }
}

impl Node for MinIoSink {
    fn name(&self) -> &str {
        "minio-sink"
    }
}

#[async_trait::async_trait]
impl Sink for MinIoSink {
    type Input = TsSegment;

    fn protocol(&self) -> Protocol {
        Protocol::Hls
    }

    fn accepted_codec(&self) -> &[CodecParams] {
        &[]
    }

    fn input(&self) -> &PadReceiver<Self::Input> {
        &self.input
    }

    fn demand_handle(&self) -> &DemandHandle {
        &self.always_wanted
    }

    async fn consume(&self, seg: Self::Input) -> Result<()> {
        // Externally-controlled live_id must be sanitized before use in keys.
        let safe_live_id = crate::sanitize::sanitize_stream_id(&self.live_id);
        let key = format!(
            "{}/{}/{}",
            self.segment_cfg.minio_prefix, safe_live_id, seg.filename
        );
        let start = std::time::Instant::now();
        let result = self.client.upload_file(&key, &seg.path).await;
        match &result {
            Ok(()) => {
                metric_minio_upload_total!("hls", "success");
                metric_minio_upload_latency_ms!("hls", "success", start.elapsed().as_millis());
                // Clean up the staged segment file after successful upload.
                if let Err(e) = std::fs::remove_file(&seg.path) {
                    tracing::warn!(path = %seg.path.display(), error = %e, "Failed to remove staged segment");
                }
                // Keep the live playlist current; HlsSegmenter writes it before
                // sending the segment. A missing playlist is ignored silently.
                self.upload_playlist(&seg.path, &safe_live_id).await;
            }
            Err(e) => {
                metric_minio_upload_total!("hls", "failure");
                // No retry mechanism exists, so a failed upload would otherwise
                // leave the staged file on disk forever (max_staged_segments is
                // not enforced here) — remove it to bound disk usage.
                tracing::warn!(path = %seg.path.display(), error = %e, "Segment upload failed, removing staged file");
                if let Err(rm_err) = std::fs::remove_file(&seg.path) {
                    tracing::warn!(path = %seg.path.display(), error = %rm_err, "Failed to remove staged segment after failed upload");
                }
            }
        }
        result
    }
}

impl MinIoSink {
    /// Upload the live playlist (index.m3u8) for this stream if it exists.
    /// Missing playlists are ignored silently; upload errors are logged.
    async fn upload_playlist(&self, segment_path: &Path, safe_live_id: &str) {
        let playlist_path = segment_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join("index.m3u8");
        if !playlist_path.is_file() {
            return;
        }
        let playlist_key = format!(
            "{}/{}/index.m3u8",
            self.segment_cfg.minio_prefix, safe_live_id
        );
        if let Err(e) = self.client.upload_file(&playlist_key, &playlist_path).await {
            tracing::warn!(
                path = %playlist_path.display(),
                error = %e,
                "Live playlist upload failed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use livestream_core::pad::PadSender;
    use parking_lot::Mutex;
    use std::path::PathBuf;
    use std::time::Duration;

    struct MockUploader {
        uploaded: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl ObjectUploader for MockUploader {
        async fn upload_file(&self, key: &str, _path: &Path) -> Result<()> {
            self.uploaded.lock().push(key.to_string());
            Ok(())
        }
    }

    struct FailingUploader;

    #[async_trait::async_trait]
    impl ObjectUploader for FailingUploader {
        async fn upload_file(&self, _key: &str, _path: &Path) -> Result<()> {
            anyhow::bail!("simulated upload failure");
        }
    }

    fn make_segment(path: PathBuf, filename: &str) -> TsSegment {
        TsSegment {
            path,
            filename: filename.into(),
            sequence: 0,
            duration: Duration::from_secs(2),
            is_final: false,
        }
    }

    #[tokio::test]
    async fn minio_sink_uploads_segment() {
        let mock = Arc::new(MockUploader {
            uploaded: Mutex::new(vec![]),
        });
        let cfg = SegmentConfig {
            duration_secs: 2,
            cache_dir: "/tmp".into(),
            playlist_size: 5,
            minio_prefix: "hls".into(),
            max_staged_segments: 10,
        };
        let (_tx, rx) = PadSender::<TsSegment>::new_channel(4);
        let sink = MinIoSink::new("test-stream", mock.clone(), cfg, rx, DemandHandle::empty());

        let seg = TsSegment {
            path: PathBuf::from("/tmp/test.ts"),
            filename: "segment_0000.ts".into(),
            sequence: 0,
            duration: Duration::from_secs(2),
            is_final: false,
        };

        sink.consume(seg).await.unwrap();
        let uploaded = mock.uploaded.lock();
        assert_eq!(uploaded.len(), 1);
        assert_eq!(uploaded[0], "hls/test-stream/segment_0000.ts");
    }

    #[tokio::test]
    async fn minio_sink_removes_staged_file_on_upload_failure() {
        let dir = tempfile::tempdir().unwrap();
        let seg_path = dir.path().join("segment_0000.ts");
        std::fs::write(&seg_path, b"ts data").unwrap();

        let cfg = SegmentConfig {
            duration_secs: 2,
            cache_dir: dir.path().display().to_string(),
            playlist_size: 5,
            minio_prefix: "hls".into(),
            max_staged_segments: 10,
        };
        let (_tx, rx) = PadSender::<TsSegment>::new_channel(4);
        let sink = MinIoSink::new(
            "test-stream",
            Arc::new(FailingUploader),
            cfg,
            rx,
            DemandHandle::empty(),
        );

        assert!(
            sink.consume(make_segment(seg_path.clone(), "segment_0000.ts"))
                .await
                .is_err()
        );
        assert!(
            !seg_path.exists(),
            "staged file should be removed after a failed upload"
        );
    }

    #[tokio::test]
    async fn minio_sink_uploads_playlist_after_segment() {
        let dir = tempfile::tempdir().unwrap();
        let seg_path = dir.path().join("segment_0000.ts");
        std::fs::write(&seg_path, b"ts data").unwrap();
        std::fs::write(dir.path().join("index.m3u8"), b"#EXTM3U\n").unwrap();

        let cfg = SegmentConfig {
            duration_secs: 2,
            cache_dir: dir.path().display().to_string(),
            playlist_size: 5,
            minio_prefix: "hls".into(),
            max_staged_segments: 10,
        };
        let mock = Arc::new(MockUploader {
            uploaded: Mutex::new(vec![]),
        });
        let (_tx, rx) = PadSender::<TsSegment>::new_channel(4);
        let sink = MinIoSink::new("test-stream", mock.clone(), cfg, rx, DemandHandle::empty());

        sink.consume(make_segment(seg_path, "segment_0000.ts"))
            .await
            .unwrap();
        let uploaded = mock.uploaded.lock();
        assert_eq!(uploaded.len(), 2);
        assert_eq!(uploaded[0], "hls/test-stream/segment_0000.ts");
        assert_eq!(uploaded[1], "hls/test-stream/index.m3u8");
    }

    #[tokio::test]
    async fn minio_sink_skips_missing_playlist() {
        let dir = tempfile::tempdir().unwrap();
        let seg_path = dir.path().join("segment_0000.ts");
        std::fs::write(&seg_path, b"ts data").unwrap();

        let cfg = SegmentConfig {
            duration_secs: 2,
            cache_dir: dir.path().display().to_string(),
            playlist_size: 5,
            minio_prefix: "hls".into(),
            max_staged_segments: 10,
        };
        let mock = Arc::new(MockUploader {
            uploaded: Mutex::new(vec![]),
        });
        let (_tx, rx) = PadSender::<TsSegment>::new_channel(4);
        let sink = MinIoSink::new("test-stream", mock.clone(), cfg, rx, DemandHandle::empty());

        sink.consume(make_segment(seg_path, "segment_0000.ts"))
            .await
            .unwrap();
        let uploaded = mock.uploaded.lock();
        assert_eq!(
            uploaded.len(),
            1,
            "missing playlist must be ignored silently"
        );
        assert_eq!(uploaded[0], "hls/test-stream/segment_0000.ts");
    }
}
