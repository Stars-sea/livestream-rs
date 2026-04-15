use anyhow::Result;
use retry::delay::{Exponential, jitter};
use std::path::PathBuf;
use std::sync::OnceLock;
use tokio::fs;
use tokio::time::sleep;
use tracing::{debug, warn};

use crate::dispatcher::{self, SessionEvent};
use crate::infra::MinioClient;
use crate::pipeline::normalize::normalize_component;

pub struct SegmentPersistenceHandler;

static PERSISTENCE_HANDLER_STARTED: OnceLock<()> = OnceLock::new();

impl SegmentPersistenceHandler {
    pub fn spawn(minio_client: MinioClient) {
        if PERSISTENCE_HANDLER_STARTED.set(()).is_err() {
            return;
        }

        Self::spawn_upload_listener(minio_client);
    }

    fn spawn_upload_listener(minio_client: MinioClient) {
        tokio::spawn(async move {
            let mut events = dispatcher::INSTANCE.subscribe_stream();

            while let Some(event) = events.next().await {
                if let SessionEvent::SegmentComplete { live_id, path } = event {
                    if let Err(e) = Self::upload_and_remove(&minio_client, &live_id, path).await {
                        warn!(live_id = %live_id, error = %e, "Failed to persist completed segment");
                    }
                }
            }
        });
    }

    async fn upload_with_retry(
        minio_client: &MinioClient,
        object_key: &str,
        segment_path: &PathBuf,
    ) -> Result<()> {
        let mut backoffs = Exponential::from_millis(200).map(jitter).take(4);

        loop {
            match minio_client.upload_file(object_key, segment_path).await {
                Ok(_) => return Ok(()),
                Err(err) => {
                    if let Some(delay) = backoffs.next() {
                        let delay_ms = delay.as_millis() as u64;
                        warn!(
                            object_key = %object_key,
                            path = %segment_path.display(),
                            delay_ms,
                            error = %err,
                            "Upload failed, scheduling retry"
                        );
                        sleep(delay).await;
                    } else {
                        return Err(err);
                    }
                }
            }
        }
    }

    async fn upload_and_remove(
        minio_client: &MinioClient,
        stream_id: &str,
        segment_path: PathBuf,
    ) -> Result<()> {
        if fs::metadata(&segment_path).await.is_err() {
            return Ok(());
        }

        let filename = segment_path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                anyhow::anyhow!("Invalid segment filename: {}", segment_path.display())
            })?;
        let object_key = format!(
            "{}/{}",
            normalize_component(stream_id),
            normalize_component(filename)
        );

        Self::upload_with_retry(minio_client, &object_key, &segment_path).await?;
        fs::remove_file(&segment_path).await?;
        debug!(stream_id = %stream_id, file = %segment_path.display(), object_key = %object_key, "Uploaded and deleted segment");

        Ok(())
    }
}
