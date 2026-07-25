//! MinIO/S3 client for uploading stream segments.

use anyhow::Result;
use minio::s3::Client;
use minio::s3::builders::ObjectContent;
use minio::s3::creds::{Provider, StaticProvider};
use minio::s3::http::BaseUrl;
use minio::s3::types::S3Api;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tracing::debug;

use crate::config::MinioConfig;

/// Client for interacting with MinIO or S3-compatible storage.
#[derive(Debug, Clone)]
pub struct PersistenceClient {
    bucket: String,
    endpoint: String,

    client: Arc<Client>,
}

impl PersistenceClient {
    pub async fn create(config: MinioConfig) -> Result<Self> {
        let base_url = config.uri.parse::<BaseUrl>()?;
        let static_provider = StaticProvider::new(&config.access_key, &config.secret_key, None);
        let provider: Box<dyn Provider + Send + Sync + 'static> = Box::new(static_provider);

        let client = Client::new(base_url, Some(provider), None, None)
            .map_err(|e| anyhow::anyhow!("Failed to create MinIO client: {}", e))?;

        let resp = client
            .bucket_exists(&config.bucket)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to check bucket existence: {}", e))?;

        if !resp.exists {
            client
                .create_bucket(&config.bucket)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create bucket: {}", e))?;
        }

        Ok(Self {
            bucket: config.bucket.clone(),
            endpoint: config.uri.clone(),
            client: client.into(),
        })
    }

    /// Uploads a file to MinIO storage.
    pub async fn upload_file(&self, filename: &str, path: &Path) -> Result<()> {
        let started = Instant::now();

        let content = ObjectContent::from(path);
        self.client
            .put_object_content(&self.bucket, filename, content)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to upload file: {}", e))?;

        let duration_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;

        debug!(
            filename = %filename,
            path = %path.display(),
            endpoint = %self.endpoint,
            duration_ms = duration_ms,
            "File uploaded"
        );
        Ok(())
    }
}
