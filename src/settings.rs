//! Application settings and configuration management.

use std::sync::OnceLock;

use anyhow::{Context, Result};
use config::{Config, Environment, File, FileFormat};
use serde::Deserialize;

/// Application settings loaded from configuration files and environment variables.
#[derive(Clone, Debug, Deserialize)]
pub struct Settings {
    /// gRPC & SRT Puller Configuration
    #[serde(default)]
    pub ingest: IngestConfig,

    /// RTMP Configuration
    #[serde(default)]
    pub publish: PublishConfig,

    /// Minio Configuration
    pub minio: Option<MinioConfig>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct IngestConfig {
    /// Host for SRT listeners (e.g., "srt.example.local")
    #[serde(default = "default_ingest_host")]
    pub host: String,

    /// Port for gRPC server to listen on
    #[serde(default = "default_ingest_grpcport")]
    pub grpcport: u16,

    /// Port for RTMP server to listen on (for pull mode)
    #[serde(default = "default_ingest_rtmpport")]
    pub rtmpport: u16,

    /// Port range for SRT listeners (format: "start-end", e.g., "4000-5000")
    #[serde(default = "default_ingest_srtports")]
    pub srtports: String,

    /// Segment duration in seconds for HLS/TS output
    #[serde(default = "default_ingest_duration")]
    pub duration: i32,

    /// Callback URL (gRPC) for stream events (e.g., stream start/stop) --- IGNORE ---
    #[serde(default)]
    pub callback: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PublishConfig {
    /// Port for RTMP server to listen on
    #[serde(default = "default_publish_port")]
    pub port: u16,

    /// RTMP application name (e.g., "live" for rtmp://host/live/streamkey)
    #[serde(default = "default_publish_appname")]
    pub appname: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MinioConfig {
    /// Base URI for MinIO/S3 (e.g., "http://localhost:9000")
    pub uri: String,

    /// Access key for MinIO/S3 authentication
    pub accesskey: String,

    /// Secret key for MinIO/S3 authentication
    pub secretkey: String,

    /// Bucket name to use for storing stream segments
    pub bucket: String,
}

fn default_ingest_host() -> String {
    "0.0.0.0".to_string()
}

fn default_ingest_grpcport() -> u16 {
    50051
}

fn default_ingest_rtmpport() -> u16 {
    1936
}

fn default_ingest_srtports() -> String {
    "4000-4100".to_string()
}

fn default_ingest_duration() -> i32 {
    10
}

fn default_publish_port() -> u16 {
    1935
}

fn default_publish_appname() -> String {
    "lives".to_string()
}

impl IngestConfig {
    /// Parses the SRT port range from the configuration.
    ///
    /// # Returns
    /// A tuple of (start_port, end_port)
    ///
    /// # Errors
    /// Returns an error if the port range format is invalid.
    pub fn srt_port_range(&self) -> Result<(u16, u16)> {
        let segments: Vec<&str> = self.srtports.split('-').collect();

        if segments.len() != 2 {
            anyhow::bail!(
                "Invalid SRT port range format '{}': expected 'start-end'",
                self.srtports
            );
        }

        let start = segments[0]
            .trim()
            .parse::<u16>()
            .with_context(|| format!("Invalid start port number: '{}'", segments[0]))?;

        let end = segments[1]
            .trim()
            .parse::<u16>()
            .with_context(|| format!("Invalid end port number: '{}'", segments[1]))?;

        Ok((start, end))
    }
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            host: default_ingest_host(),
            grpcport: default_ingest_grpcport(),
            rtmpport: default_ingest_rtmpport(),
            srtports: default_ingest_srtports(),
            duration: default_ingest_duration(),
            callback: "".to_string(),
        }
    }
}

impl Default for PublishConfig {
    fn default() -> Self {
        Self {
            port: default_publish_port(),
            appname: default_publish_appname(),
        }
    }
}

impl Settings {
    /// Loads settings from configuration files and environment variables.
    pub fn new() -> Result<Self> {
        let builder = Config::builder()
            .add_source(File::new("config.toml", FileFormat::Toml).required(false))
            .add_source(Environment::default().try_parsing(true).separator("_"));

        let config = builder.build().context("Failed to build configuration")?;

        let settings: Settings = config
            .try_deserialize()
            .context("Failed to deserialize configuration")?;

        settings.validate()?;

        Ok(settings)
    }

    /// Validates the configuration settings.
    pub fn validate(&self) -> Result<()> {
        let (start, end) = self.ingest.srt_port_range()?;
        if start >= end {
            anyhow::bail!(
                "Invalid SRT port range: start port {} must be less than end port {}",
                start,
                end
            );
        }

        if self.ingest.duration <= 0 {
            anyhow::bail!("Segment duration must be positive");
        }

        if self.publish.port == self.ingest.rtmpport {
            anyhow::bail!("RTMP publish port must be different from RTMP port");
        }

        if self.minio.is_none() {
            anyhow::bail!(
                "MinIO configuration is missing: please set minio.uri, minio.accesskey, minio.secretkey, and minio.bucket"
            );
        }

        Ok(())
    }
}

pub fn load_settings() -> &'static Settings {
    static SETTINGS: OnceLock<Settings> = OnceLock::new();

    &SETTINGS.get_or_init(|| Settings::new().expect("Failed to load application settings"))
}
