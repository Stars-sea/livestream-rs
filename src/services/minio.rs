//! MinIO/S3 client for uploading stream segments.

use anyhow::Result;
use minio::s3::Client;
use minio::s3::builders::ObjectContent;
use minio::s3::creds::StaticProvider;
use minio::s3::http::BaseUrl;
use minio::s3::types::S3Api;
use std::path::Path;
use std::sync::Arc;
use tracing::debug;

use crate::settings::MinioConfig;

/// Client for interacting with MinIO or S3-compatible storage.
#[derive(Debug, Clone)]
pub struct MinioClient {
    bucket: String,

    client: Arc<Client>,
}

impl MinioClient {
    pub async fn create(config: MinioConfig) -> Result<Self> {
        let base_url = config.uri.parse::<BaseUrl>()?;
        let static_provider = StaticProvider::new(&config.accesskey, &config.secretkey, None);

        let client = Client::new(base_url, Some(Box::new(static_provider)), None, None)?;

        let exists_resp = client.bucket_exists(config.bucket.as_str()).send().await;
        if exists_resp.is_err() || !exists_resp?.exists {
            client.create_bucket(config.bucket.as_str()).send().await?;
        }
        Ok(MinioClient {
            bucket: config.bucket.clone(),
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

        debug!(filename = %filename, path = %path.display(), "File uploaded");
        Ok(())
    }
}
