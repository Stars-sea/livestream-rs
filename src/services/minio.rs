//! MinIO/S3 client for uploading stream segments.

use anyhow::Result;
use minio::s3::Client;
use minio::s3::builders::ObjectContent;
use minio::s3::creds::StaticProvider;
use minio::s3::http::BaseUrl;
use minio::s3::types::S3Api;
use std::path::Path;
use std::sync::Arc;
use tracing::{Instrument, debug, info_span};

use crate::settings::{MinioConfig, load_settings};

/// Client for interacting with MinIO or S3-compatible storage.
#[derive(Debug, Clone)]
pub struct MinioClient {
    bucket: String,
    endpoint: String,

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
        let request_span = info_span!(
            "minio.upload_file",
            otel.kind = "client",
            storage.bucket.name = %self.bucket,
            object.key = %filename,
            file.path = %path.display(),
            server.address = %self.endpoint
        );

        self.client
            .put_object_content(self.bucket.as_str(), filename, ObjectContent::from(path))
            .send()
            .instrument(request_span)
            .await?;

        debug!(filename = %filename, path = %path.display(), "File uploaded");
        Ok(())
    }
}

impl Default for MinioClient {
    fn default() -> Self {
        let config = load_settings()
            .minio
            .clone()
            .expect("Failed to load Minio configuration");

        tokio::runtime::Handle::current()
            .block_on(Self::create(config))
            .expect("Failed to create Minio client")
    }
}
