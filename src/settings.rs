//! Application settings and configuration management.

use anyhow::{Context, Result};
use config::{Config, Environment, File, FileFormat};
use serde::Deserialize;

/// Application settings loaded from configuration files and environment variables.
#[derive(Clone, Debug, Deserialize)]
pub struct Settings {
    /// Host for SRT listeners (e.g., "srt.example.local")
    #[serde(default = "default_host")]
    pub host: String,

    /// Port range for SRT listeners (format: "start-end", e.g., "4000-5000")
    #[serde(default = "default_srt_ports")]
    pub srt_ports: String,

    /// gRPC Server Port
    #[serde(default = "default_grpc_port")]
    pub grpc_port: u16,

    /// gRPC callback URL for stream events (e.g., "http://localhost:50051")
    #[serde(default, alias = "LIVE_SVC_GRPC")]
    pub grpc_callback: String,

    /// Segment duration in seconds for HLS/TS output
    #[serde(default = "default_segment_time")]
    pub segment_time: i32,

    /// RTMP Server Port
    #[serde(default = "default_rtmp_port")]
    pub rtmp_port: u16,

    /// RTMP Application Name
    #[serde(default = "default_rtmp_app")]
    pub rtmp_app: String,

    /// Minio Endpoint URL
    #[serde(default = "default_minio_uri")]
    pub minio_uri: String,

    /// Minio Access Key
    #[serde(default = "default_minio_accesskey")]
    pub minio_accesskey: String,

    /// Minio Secret Key
    #[serde(default = "default_minio_secretkey")]
    pub minio_secretkey: String,

    /// Minio Bucket Name
    #[serde(default = "default_minio_bucket")]
    pub minio_bucket: String,
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}
fn default_srt_ports() -> String {
    "4000-4100".to_string()
}
fn default_grpc_port() -> u16 {
    50051
}
fn default_segment_time() -> i32 {
    10
}
fn default_rtmp_port() -> u16 {
    1935
}
fn default_rtmp_app() -> String {
    "lives".to_string()
}
fn default_minio_uri() -> String {
    "http://localhost:9000".to_string()
}
fn default_minio_accesskey() -> String {
    "minioadmin".to_string()
}
fn default_minio_secretkey() -> String {
    "minioadmin".to_string()
}
fn default_minio_bucket() -> String {
    "livestream".to_string()
}

impl Settings {
    /// Loads settings from configuration files and environment variables.
    pub fn new() -> Result<Self> {
        let builder = Config::builder()
            .add_source(File::new("config", FileFormat::Toml).required(false))
            .add_source(File::new("config.local", FileFormat::Toml).required(false))
            .add_source(Environment::default().try_parsing(true).separator("__"));

        let config = builder.build().context("Failed to build configuration")?;

        let settings: Settings = config
            .try_deserialize()
            .context("Failed to deserialize configuration")?;

        settings.validate()?;

        Ok(settings)
    }

    /// Validates the configuration settings.
    pub fn validate(&self) -> Result<()> {
        let (start, end) = self.srt_port_range()?;
        if start >= end {
            anyhow::bail!(
                "Invalid SRT port range: start port {} must be less than end port {}",
                start,
                end
            );
        }

        if self.segment_time <= 0 {
            anyhow::bail!("Segment time must be positive");
        }

        Ok(())
    }

    /// Parses the SRT port range from the configuration.
    ///
    /// # Returns
    /// A tuple of (start_port, end_port)
    ///
    /// # Errors
    /// Returns an error if the port range format is invalid.
    pub fn srt_port_range(&self) -> Result<(u16, u16)> {
        let segments: Vec<&str> = self.srt_ports.split('-').collect();

        if segments.len() != 2 {
            anyhow::bail!(
                "Invalid SRT port range format '{}': expected 'start-end'",
                self.srt_ports
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
