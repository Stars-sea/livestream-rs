//! MinIO/S3 client for uploading stream segments.

use anyhow::Result;
use minio::s3::builders::ObjectContent;
use minio::s3::creds::StaticProvider;
use minio::s3::http::BaseUrl;
use minio::s3::types::S3Api;
use minio::s3::{MinioClient, MinioClientBuilder};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tracing::debug;

use crate::{config::MinioConfig, metric_minio_upload_latency_ms, metric_minio_upload_total};

/// Client for interacting with MinIO or S3-compatible storage.
#[derive(Debug, Clone)]
pub struct PersistenceClient {
    bucket: String,
    endpoint: String,

    client: Arc<MinioClient>,
}

impl PersistenceClient {
    pub async fn create(config: MinioConfig) -> Result<Self> {
        let base_url = config.uri.parse::<BaseUrl>()?;
        let static_provider = StaticProvider::new(&config.accesskey, &config.secretkey, None);

        let client = MinioClientBuilder::new(base_url)
            .provider(Some(static_provider))
            .build()?;

        let exists_resp = client
            .bucket_exists(config.bucket.as_str())?
            .build()
            .send()
            .await;
        if exists_resp.is_err() || !exists_resp?.exists() {
            client
                .create_bucket(config.bucket.as_str())?
                .build()
                .send()
                .await?;
        }
        Ok(Self {
            bucket: config.bucket.clone(),
            endpoint: config.uri.clone(),
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
        let started = Instant::now();

        let upload_result = self
            .client
            .put_object_content(self.bucket.as_str(), filename, ObjectContent::from(path))?
            .build()
            .send()
            .await;

        let duration_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let status = if upload_result.is_ok() { "ok" } else { "error" };
        metric_minio_upload_total!(self.bucket.as_str(), status);
        metric_minio_upload_latency_ms!(self.bucket.as_str(), status, duration_ms);

        upload_result?;

        debug!(
            filename = %filename,
            path = %path.display(),
            endpoint = %self.endpoint,
            duration_ms = duration_ms,
            "File uploaded"
        );
        Ok(())
    }

    // TODO: Add methods for listing objects, deleting objects, etc.

    // TODO: Upload from in-memory buffers for more efficient streaming uploads.
}
