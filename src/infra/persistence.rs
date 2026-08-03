//! MinIO/S3 client for uploading stream segments.

use anyhow::{Context, Result};
use minio::s3::Client;
use minio::s3::builders::ObjectContent;
use minio::s3::creds::{Provider, StaticProvider};
use minio::s3::error::{Error as MinioError, ErrorCode};
use minio::s3::http::BaseUrl;
use minio::s3::types::S3Api;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tracing::debug;

use crate::config::MinioConfig;
use livestream_pipeline::sink::minio::ObjectUploader;

#[async_trait::async_trait]
impl ObjectUploader for PersistenceClient {
    async fn upload_file(&self, object_key: &str, file_path: &Path) -> Result<()> {
        PersistenceClient::upload_file(self, object_key, file_path).await
    }
}

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
            .with_context(|| "Failed to create MinIO client")?;

        ensure_bucket(&client, &config.bucket).await?;

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
            .with_context(|| format!("Failed to upload file '{}'", filename))?;

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

/// Ensure the target bucket exists, creating it when missing.
///
/// Bucket creation is intentionally tolerant of concurrent instances: the
/// check-then-act window between `bucket_exists` and `create_bucket` can be
/// raced by another process, and "already exists" errors from that race are
/// treated as success.
async fn ensure_bucket(client: &Client, bucket: &str) -> Result<()> {
    let resp = client
        .bucket_exists(bucket)
        .send()
        .await
        .with_context(|| format!("Failed to check existence of bucket '{bucket}'"))?;
    if resp.exists {
        return Ok(());
    }

    match client.create_bucket(bucket).send().await {
        Ok(_) => Ok(()),
        Err(e) if is_bucket_already_exists(&e) => Ok(()),
        Err(e) => Err(e).with_context(|| format!("Failed to create bucket '{bucket}'")),
    }
}

/// Whether a bucket-creation error means the bucket already exists (a benign
/// race with a concurrent instance, not a real failure).
fn is_bucket_already_exists(err: &MinioError) -> bool {
    if let MinioError::S3Error(e) = err {
        return e.code == ErrorCode::BucketAlreadyOwnedByYou
            || matches!(
                &e.code,
                ErrorCode::OtherError(code)
                    if code.eq_ignore_ascii_case("BucketAlreadyExists")
            );
    }
    false
}
