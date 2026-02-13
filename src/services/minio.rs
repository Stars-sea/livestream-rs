//! MinIO/S3 client for uploading stream segments.

use anyhow::Result;
use log::{debug, info, warn};
use minio::s3::Client;
use minio::s3::builders::ObjectContent;
use minio::s3::creds::StaticProvider;
use minio::s3::http::BaseUrl;
use minio::s3::types::S3Api;
use std::path::Path;
use std::sync::Arc;
use tokio::fs;
use tokio_stream::StreamExt;

use crate::livestream::events::SegmentCompleteStream;

/// Client for interacting with MinIO or S3-compatible storage.
#[derive(Debug, Clone)]
pub struct MinioClient {
    bucket: String,

    client: Arc<Client>,
}

impl MinioClient {
    /// Creates a new MinIO client and ensures the bucket exists.
    ///
    /// # Arguments
    /// * `endpoint` - MinIO server endpoint URL
    /// * `access_key` - Access key for authentication
    /// * `secret_key` - Secret key for authentication
    /// * `bucket` - Bucket name to use
    ///
    /// # Errors
    /// Returns an error if connection fails or bucket cannot be created.
    pub async fn create(
        endpoint: &str,
        access_key: &str,
        secret_key: &str,
        bucket: &str,
    ) -> Result<Self> {
        let base_url = endpoint.parse::<BaseUrl>()?;
        let static_provider = StaticProvider::new(access_key.into(), secret_key.into(), None);

        let client = Client::new(base_url, Some(Box::new(static_provider)), None, None)?;

        let exists_resp = client.bucket_exists(bucket).send().await;
        if exists_resp.is_err() || !exists_resp?.exists {
            client.create_bucket(bucket).send().await?;
        }
        Ok(Self {
            bucket: bucket.into(),
            client: client.into(),
        })
    }

    /// Uploads a file to MinIO storage.
    ///
    /// # Arguments
    /// * `filename` - Object key/name in the bucket
    /// * `path` - Local file path to upload
    ///
    /// # Errors
    /// Returns an error if upload fails.
    pub async fn upload_file(&self, filename: &str, path: &Path) -> Result<()> {
        self.client
            .put_object_content(self.bucket.as_str(), filename, ObjectContent::from(path))
            .send()
            .await?;

        debug!("File {} uploaded", filename);
        Ok(())
    }
}

pub async fn minio_uploader(
    mut stream: SegmentCompleteStream,
    minio: MinioClient,
) -> anyhow::Result<()> {
    while let Some(complete_info) = stream.next().await {
        let path = complete_info.path();
        info!("Uploading file {}", path.display());

        let storage_key = format!("{}/{}", complete_info.live_id(), complete_info.segment_id());
        let upload_resp = minio
            .upload_file(
                storage_key.as_str(),
                fs::canonicalize(&path).await?.as_path(),
            )
            .await;

        if let Err(e) = upload_resp {
            warn!("Upload failed for {}: {:?}", path.display(), e);
            continue;
        }

        debug!("Remove file {}", path.display());
        if fs::remove_file(&path).await.is_err() {
            warn!("Failed to remove file {}", path.display());
        }
    }
    Ok(())
}
