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
        let key = format!(
            "{}/{}/{}",
            self.segment_cfg.minio_prefix, self.live_id, seg.filename
        );
        let result = self.client.upload_file(&key, &seg.path).await;
        if result.is_ok() {
            // Clean up the staged segment file after successful upload.
            if let Err(e) = std::fs::remove_file(&seg.path) {
                tracing::warn!(path = %seg.path.display(), error = %e, "Failed to remove staged segment");
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use livestream_core::pad::PadSender;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;

    struct MockUploader {
        uploaded: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl ObjectUploader for MockUploader {
        async fn upload_file(&self, key: &str, _path: &Path) -> Result<()> {
            self.uploaded.lock().unwrap().push(key.to_string());
            Ok(())
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
        let uploaded = mock.uploaded.lock().unwrap();
        assert_eq!(uploaded.len(), 1);
        assert_eq!(uploaded[0], "hls/test-stream/segment_0000.ts");
    }
}
