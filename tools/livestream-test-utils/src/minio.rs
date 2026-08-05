//! MinIO connection string parsing and HLS persistence verification.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use minio::s3::Client;
use minio::s3::creds::StaticProvider;
use minio::s3::http::BaseUrl;
use minio::s3::types::ToStream;

/// HLS object key prefix; must match the server's `SegmentConfig::minio_prefix`
/// default and LiveService's `LivestreamOptions__SegmentPrefix` ("hls").
pub const HLS_PREFIX: &str = "hls";

/// Max wall-clock time to wait for the first uploaded segment + playlist.
pub const HLS_VERIFY_TIMEOUT: Duration = Duration::from_secs(12);
/// Poll interval while waiting for HLS objects.
pub const HLS_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// MinIO access configuration, parsed from an Aspire-style connection string.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MinioConfig {
    pub endpoint: String,
    pub access_key: String,
    pub secret_key: String,
    pub bucket: String,
}

/// Parse `Endpoint=...;AccessKey=...;SecretKey=...` (key names case-insensitive,
/// `;`-separated `key=value` pairs, values trimmed). Unknown keys are ignored.
/// `endpoint`/`accesskey`/`secretkey` are required; `bucket` falls back to
/// `default_bucket`.
pub fn parse_connection_string(conn: &str, default_bucket: &str) -> Result<MinioConfig> {
    let mut endpoint = None;
    let mut access_key = None;
    let mut secret_key = None;
    let mut bucket = None;
    for pair in conn.split(';') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        let key = k.trim().to_ascii_lowercase();
        let value = v.trim();
        match key.as_str() {
            "endpoint" => endpoint = Some(value.to_string()),
            "accesskey" => access_key = Some(value.to_string()),
            "secretkey" => secret_key = Some(value.to_string()),
            "bucket" => bucket = Some(value.to_string()),
            _ => {}
        }
    }
    let endpoint = endpoint.context("missing Endpoint")?;
    let access_key = access_key.context("missing AccessKey")?;
    let secret_key = secret_key.context("missing SecretKey")?;
    if endpoint.is_empty() || access_key.is_empty() || secret_key.is_empty() {
        bail!("Endpoint/AccessKey/SecretKey must not be empty");
    }
    Ok(MinioConfig {
        endpoint,
        access_key,
        secret_key,
        bucket: bucket
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| default_bucket.to_string()),
    })
}

fn build_client(cfg: &MinioConfig) -> Result<Client> {
    let base_url = cfg
        .endpoint
        .parse::<BaseUrl>()
        .context("invalid Endpoint URL")?;
    let provider = StaticProvider::new(&cfg.access_key, &cfg.secret_key, None);
    Client::new(base_url, Some(Box::new(provider)), None, None)
        .context("failed to create MinIO client")
}

/// Outcome of the HLS persistence check.
#[derive(Debug, Clone, Copy)]
pub struct HlsVerification {
    pub verified: bool,
    pub segments: usize,
}

/// Poll MinIO until `{HLS_PREFIX}/{live_id}/index.m3u8` and at least one
/// `segment_*.ts` object exist, or `HLS_VERIFY_TIMEOUT` elapses. List errors
/// are returned as `Err`; a clean timeout returns `verified: false`.
pub async fn verify_hls(cfg: &MinioConfig, live_id: &str) -> Result<HlsVerification> {
    let client = build_client(cfg)?;
    let prefix = format!("{HLS_PREFIX}/{live_id}/");
    let playlist_key = format!("{prefix}index.m3u8");
    let deadline = tokio::time::Instant::now() + HLS_VERIFY_TIMEOUT;
    loop {
        let mut stream = client
            .list_objects(&cfg.bucket)
            .prefix(Some(prefix.clone()))
            .recursive(true)
            .to_stream()
            .await;
        // A single page (max 1000 objects) is enough for verification.
        let resp = match stream.next().await {
            Some(Ok(resp)) => resp,
            Some(Err(e)) => bail!("list HLS objects failed: {e}"),
            None => bail!("list HLS objects returned no pages"),
        };
        let keys: Vec<&str> = resp.contents.iter().map(|e| e.name.as_str()).collect();
        let has_playlist = keys.iter().any(|k| *k == playlist_key);
        let segments = keys.iter().filter(|k| k.ends_with(".ts")).count();
        if has_playlist && segments > 0 {
            return Ok(HlsVerification {
                verified: true,
                segments,
            });
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(HlsVerification {
                verified: false,
                segments,
            });
        }
        tokio::time::sleep(HLS_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_toolkit_connection_string() {
        let cfg = parse_connection_string(
            "Endpoint=http://localhost:9000;AccessKey=minioadmin;SecretKey=miniokey",
            "videos",
        )
        .unwrap();
        assert_eq!(cfg.endpoint, "http://localhost:9000");
        assert_eq!(cfg.access_key, "minioadmin");
        assert_eq!(cfg.secret_key, "miniokey");
        assert_eq!(cfg.bucket, "videos");
    }

    #[test]
    fn parses_lowercase_keys_and_inline_bucket() {
        let cfg = parse_connection_string(
            "endpoint=http://h:9000;accesskey=a;secretkey=s;bucket=b",
            "videos",
        )
        .unwrap();
        assert_eq!(cfg.bucket, "b");
    }

    #[test]
    fn rejects_missing_credentials() {
        assert!(parse_connection_string("Endpoint=http://h:9000;AccessKey=a", "videos").is_err());
    }
}
