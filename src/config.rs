//! Application settings and configuration management.

use std::sync::OnceLock;

use anyhow::{Context, Result};
use config::{Config, Environment, File, FileFormat};
use serde::Deserialize;

/// Application settings loaded from configuration files and environment variables.
#[derive(Clone, Debug, Deserialize)]
pub struct AppConfig {
    /// Srt Configuration
    #[serde(default)]
    pub srt: SrtConfig,

    /// gRPC Configuration
    #[serde(default)]
    pub grpc: GrpcConfig,

    /// RTMP Configuration
    #[serde(default)]
    pub rtmp: RtmpConfig,

    /// Queue capacity configuration
    #[serde(default)]
    pub queue: QueueConfig,

    /// Minio Configuration
    pub minio: Option<MinioConfig>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SrtConfig {
    /// Port range for SRT listeners (format: "start-end", e.g., "4000-5000")
    #[serde(default = "default_srt_srtports")]
    pub srtports: String,

    /// Segment duration in seconds for HLS/TS output
    /// TODO: Consider moving to persistence configuration
    #[serde(default = "default_srt_duration")]
    pub duration: i32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GrpcConfig {
    /// Port for gRPC server to listen on
    #[serde(default = "default_grpc_port")]
    pub port: u16,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RtmpConfig {
    /// Port for RTMP server to listen on
    #[serde(default = "default_rtmp_port")]
    pub port: u16,

    /// RTMP application name (e.g., "live" for rtmp://host/live/streamkey)
    #[serde(default = "default_rtmp_appname")]
    pub appname: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct QueueConfig {
    /// Capacity for RTMP forwarded tag queue from pipeline to transport
    #[serde(default = "default_rtmp_forward_queue_capacity")]
    pub rtmp_forward: usize,

    /// Capacity for internal FLV relay queue in FLV mux middleware
    #[serde(default = "default_flv_relay_queue_capacity")]
    pub flv_relay: usize,

    /// Capacity for RTMP publish tag queue
    #[serde(default = "default_rtmp_publish_queue_capacity")]
    pub rtmp_publish: usize,

    /// Capacity for SRT packet queue
    #[serde(default = "default_srt_packet_queue_capacity")]
    pub srt_packet: usize,

    /// Capacity for transport control queues (RTMP/SRT)
    #[serde(default = "default_control_queue_capacity")]
    pub control: usize,

    /// Capacity for transport event queue
    #[serde(default = "default_event_queue_capacity")]
    pub event: usize,
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

fn default_grpc_port() -> u16 {
    50051
}

fn default_srt_srtports() -> String {
    "4000-4100".to_string()
}

fn default_srt_duration() -> i32 {
    10
}

fn default_rtmp_port() -> u16 {
    1935
}

fn default_rtmp_appname() -> String {
    "lives".to_string()
}

fn default_rtmp_forward_queue_capacity() -> usize {
    8192
}

fn default_flv_relay_queue_capacity() -> usize {
    2048
}

fn default_rtmp_publish_queue_capacity() -> usize {
    4096
}

fn default_srt_packet_queue_capacity() -> usize {
    4096
}

fn default_control_queue_capacity() -> usize {
    1024
}

fn default_event_queue_capacity() -> usize {
    4096
}

impl SrtConfig {
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

impl Default for SrtConfig {
    fn default() -> Self {
        Self {
            srtports: default_srt_srtports(),
            duration: default_srt_duration(),
        }
    }
}

impl Default for GrpcConfig {
    fn default() -> Self {
        Self {
            port: default_grpc_port(),
        }
    }
}

impl Default for RtmpConfig {
    fn default() -> Self {
        Self {
            port: default_rtmp_port(),
            appname: default_rtmp_appname(),
        }
    }
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            rtmp_forward: default_rtmp_forward_queue_capacity(),
            flv_relay: default_flv_relay_queue_capacity(),
            rtmp_publish: default_rtmp_publish_queue_capacity(),
            srt_packet: default_srt_packet_queue_capacity(),
            control: default_control_queue_capacity(),
            event: default_event_queue_capacity(),
        }
    }
}

impl AppConfig {
    /// Loads settings from configuration files and environment variables.
    pub fn new() -> Result<Self> {
        let builder = Config::builder()
            .add_source(File::new("config.toml", FileFormat::Toml).required(false))
            .add_source(Environment::default().try_parsing(true).separator("_"));

        let config = builder.build().context("Failed to build configuration")?;

        let settings: AppConfig = config
            .try_deserialize()
            .context("Failed to deserialize configuration")?;

        settings.validate()?;

        Ok(settings)
    }

    /// Validates the configuration settings.
    pub fn validate(&self) -> Result<()> {
        let (start, end) = self.srt.srt_port_range()?;
        if start >= end {
            anyhow::bail!(
                "Invalid SRT port range: start port {} must be less than end port {}",
                start,
                end
            );
        }

        if self.srt.duration <= 0 {
            anyhow::bail!("Segment duration must be positive");
        }

        if self.queue.rtmp_forward == 0
            || self.queue.flv_relay == 0
            || self.queue.rtmp_publish == 0
            || self.queue.srt_packet == 0
            || self.queue.control == 0
            || self.queue.event == 0
        {
            anyhow::bail!("All queue capacities must be greater than 0");
        }

        if !cfg!(test) && self.minio.is_none() {
            anyhow::bail!(
                "MinIO configuration is missing: please set minio.uri, minio.accesskey, minio.secretkey, and minio.bucket"
            );
        }

        Ok(())
    }
}

pub fn load_config() -> &'static AppConfig {
    static SETTINGS: OnceLock<AppConfig> = OnceLock::new();

    &SETTINGS.get_or_init(|| AppConfig::new().expect("Failed to load application settings"))
}
