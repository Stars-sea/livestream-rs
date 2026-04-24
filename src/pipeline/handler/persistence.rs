use anyhow::Result;
use retry::delay::{Exponential, jitter};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::fs;
use tokio::time::sleep;
use tracing::{debug, warn};

use crate::dispatcher::{self, SessionEvent};
use crate::infra::PersistenceClient;
use crate::pipeline::normalize::normalize_component;

pub struct SegmentPersistenceHandler;

static PERSISTENCE_HANDLER_STARTED: OnceLock<()> = OnceLock::new();

impl SegmentPersistenceHandler {
    pub fn spawn(minio_client: PersistenceClient) {
        if PERSISTENCE_HANDLER_STARTED.set(()).is_err() {
            return;
        }

        Self::spawn_upload_listener(minio_client);
    }

    fn spawn_upload_listener(minio_client: PersistenceClient) {
        tokio::spawn(async move {
            let mut events = dispatcher::INSTANCE.subscribe_global();

            while let Some(event) = events.next().await {
                match event {
                    SessionEvent::SegmentComplete { live_id, path } => {
                        if let Err(e) = Self::upload_and_remove(&minio_client, &live_id, path).await
                        {
                            warn!(live_id = %live_id, error = %e, "Failed to persist completed segment");
                        }
                    }
                    SessionEvent::PlaylistUpdated {
                        live_id,
                        path,
                        is_final,
                    } => {
                        if let Err(e) =
                            Self::upload_playlist(&minio_client, &live_id, path, is_final).await
                        {
                            warn!(live_id = %live_id, error = %e, "Failed to persist playlist");
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    async fn upload_with_retry(
        minio_client: &PersistenceClient,
        object_key: &str,
        file_path: &Path,
    ) -> Result<()> {
        let mut backoffs = Exponential::from_millis(200).map(jitter).take(4);

        loop {
            match minio_client.upload_file(object_key, file_path).await {
                Ok(_) => return Ok(()),
                Err(err) => {
                    if let Some(delay) = backoffs.next() {
                        let delay_ms = delay.as_millis() as u64;
                        warn!(
                            object_key = %object_key,
                            path = %file_path.display(),
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
        minio_client: &PersistenceClient,
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

    async fn upload_playlist(
        minio_client: &PersistenceClient,
        stream_id: &str,
        playlist_path: PathBuf,
        is_final: bool,
    ) -> Result<()> {
        if fs::metadata(&playlist_path).await.is_err() {
            return Ok(());
        }

        let object_key = format!("{}/index.m3u8", normalize_component(stream_id));
        Self::upload_with_retry(minio_client, &object_key, &playlist_path).await?;

        let should_remove_local = is_final
            || playlist_path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name != "index.m3u8");

        if should_remove_local {
            fs::remove_file(&playlist_path).await?;
            debug!(stream_id = %stream_id, file = %playlist_path.display(), object_key = %object_key, "Uploaded and deleted playlist file");
        } else {
            debug!(stream_id = %stream_id, file = %playlist_path.display(), object_key = %object_key, "Uploaded playlist");
        }

        Ok(())
    }
}
