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

    /// Persistence configuration
    #[serde(default)]
    pub persistence: PersistenceConfig,

    /// Queue capacity configuration
    #[serde(default)]
    pub queue: QueueConfig,

    /// Minio Configuration
    pub minio: Option<MinioConfig>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SrtConfig {
    /// Port range for SRT listeners (format: "start-end", e.g., "4000-5000")
    #[serde(default = "default_srt_ports")]
    pub ports: String,
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

    /// TTL in seconds for precreated RTMP sessions that never get published.
    #[serde(default = "default_rtmp_ttl")]
    pub ttl: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PersistenceConfig {
    /// Segment duration in seconds for HLS/TS output
    #[serde(default = "default_persistence_duration")]
    pub duration: i32,

    /// Optional parent cache directory for segment temp files.
    /// Empty value means using system temp directory.
    #[serde(default = "default_persistence_cachedir")]
    pub cachedir: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct QueueConfig {
    /// Capacity for RTMP forwarded tag queue from pipeline to transport
    #[serde(default = "default_rtmp_forward_queue_capacity")]
    pub rtmpforward: usize,

    /// Capacity for internal FLV relay queue in FLV mux middleware
    #[serde(default = "default_flv_relay_queue_capacity")]
    pub flvrelay: usize,

    /// Capacity for internal AVPacket relay queue in SRT connection handler
    #[serde(default = "default_packet_relay_queue_capacity")]
    pub packetrelay: usize,

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

fn default_srt_ports() -> String {
    "4000-4100".to_string()
}

fn default_persistence_duration() -> i32 {
    10
}

fn default_persistence_cachedir() -> String {
    "".to_string()
}

fn default_rtmp_port() -> u16 {
    1935
}

fn default_rtmp_appname() -> String {
    "lives".to_string()
}

fn default_rtmp_ttl() -> u64 {
    30
}

fn default_rtmp_forward_queue_capacity() -> usize {
    8192
}

fn default_flv_relay_queue_capacity() -> usize {
    2048
}

fn default_control_queue_capacity() -> usize {
    1024
}

fn default_event_queue_capacity() -> usize {
    4096
}

fn default_packet_relay_queue_capacity() -> usize {
    2048
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
        let segments: Vec<&str> = self.ports.split('-').collect();

        if segments.len() != 2 {
            anyhow::bail!(
                "Invalid SRT port range format '{}': expected 'start-end'",
                self.ports
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
            ports: default_srt_ports(),
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
            ttl: default_rtmp_ttl(),
        }
    }
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            duration: default_persistence_duration(),
            cachedir: default_persistence_cachedir(),
        }
    }
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            rtmpforward: default_rtmp_forward_queue_capacity(),
            flvrelay: default_flv_relay_queue_capacity(),
            control: default_control_queue_capacity(),
            event: default_event_queue_capacity(),
            packetrelay: default_packet_relay_queue_capacity(),
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

        if self.persistence.duration <= 0 {
            anyhow::bail!("Segment duration must be positive");
        }

        const MAX_RTMP_SESSION_TTL_SECS: u64 = 86_400;
        if self.rtmp.ttl > MAX_RTMP_SESSION_TTL_SECS {
            anyhow::bail!(
                "RTMP session TTL must be in 0..={} seconds, got {}",
                MAX_RTMP_SESSION_TTL_SECS,
                self.rtmp.ttl
            );
        }

        let cache_dir = self.persistence.cachedir.trim();
        if cache_dir == "." || cache_dir == ".." {
            anyhow::bail!("Persistence cachedir cannot be '.' or '..'");
        }

        if self.queue.rtmpforward == 0
            || self.queue.flvrelay == 0
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
