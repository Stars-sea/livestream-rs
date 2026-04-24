//! Application settings and configuration management.

use std::sync::OnceLock;

use anyhow::{Context, Result};
use config::{Config, Environment, File, FileFormat};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct AppConfig {
    #[serde(default, flatten)]
    pub transport: TransportConfig,

    #[serde(default, flatten)]
    pub services: ServiceConfig,

    #[serde(default, flatten)]
    pub storage: StorageConfig,

    #[serde(default)]
    pub queue: QueueConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct TransportConfig {
    #[serde(default)]
    pub srt: SrtConfig,

    #[serde(default)]
    pub rtmp: RtmpConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ServiceConfig {
    #[serde(default)]
    pub grpc: GrpcConfig,

    #[serde(default)]
    pub http_flv: HttpFlvConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct StorageConfig {
    #[serde(default)]
    pub persistence: PersistenceConfig,

    pub minio: Option<MinioConfig>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SrtConfig {
    #[serde(default = "default_srt_ports")]
    pub ports: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GrpcConfig {
    #[serde(default = "default_grpc_port")]
    pub port: u16,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RtmpConfig {
    #[serde(default = "default_rtmp_port")]
    pub port: u16,

    #[serde(default = "default_rtmp_app_name")]
    pub app_name: String,

    #[serde(default = "default_rtmp_session_ttl_secs")]
    pub session_ttl_secs: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct HttpFlvConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_http_flv_port")]
    pub port: u16,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PersistenceConfig {
    #[serde(default = "default_persistence_duration")]
    pub duration: i32,

    #[serde(default = "default_persistence_cache_dir")]
    pub cache_dir: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct QueueConfig {
    #[serde(default = "default_rtmp_forward_queue_capacity")]
    pub rtmp_forward: usize,

    #[serde(default = "default_flv_relay_queue_capacity")]
    pub flv_relay: usize,

    #[serde(default = "default_packet_relay_queue_capacity")]
    pub packet_relay: usize,

    #[serde(default = "default_control_queue_capacity")]
    pub control: usize,

    #[serde(default = "default_event_queue_capacity")]
    pub event: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MinioConfig {
    pub uri: String,

    pub access_key: String,

    pub secret_key: String,

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

fn default_persistence_cache_dir() -> String {
    "".to_string()
}

fn default_rtmp_port() -> u16 {
    1935
}

fn default_rtmp_app_name() -> String {
    "lives".to_string()
}

fn default_rtmp_session_ttl_secs() -> u64 {
    30
}

fn default_http_flv_port() -> u16 {
    8080
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

impl AppConfig {
    pub fn new() -> Result<Self> {
        let mut builder = Config::builder()
            .add_source(File::new("config.toml", FileFormat::Toml).required(false))
            .add_source(Environment::default().try_parsing(true).separator("__"));

        builder = builder
            .set_override_option("minio.uri", std::env::var("MINIO_URI").ok())?
            .set_override_option("minio.access_key", std::env::var("MINIO_ACCESSKEY").ok())?
            .set_override_option("minio.secret_key", std::env::var("MINIO_SECRETKEY").ok())?
            .set_override_option("minio.bucket", std::env::var("MINIO_BUCKET").ok())?;

        let config = builder.build().context("Failed to build configuration")?;

        let settings: AppConfig = config
            .try_deserialize()
            .context("Failed to deserialize configuration")?;

        settings.validate()?;

        Ok(settings)
    }

    pub fn validate(&self) -> Result<()> {
        self.transport.validate()?;
        self.storage.validate()?;
        self.queue.validate()?;
        Ok(())
    }
}

impl TransportConfig {
    fn validate(&self) -> Result<()> {
        self.srt.validate()?;
        self.rtmp.validate()?;
        Ok(())
    }
}

impl StorageConfig {
    fn validate(&self) -> Result<()> {
        self.persistence.validate()?;

        if !cfg!(test) && self.minio.is_none() {
            anyhow::bail!(
                "MinIO configuration is missing: please set minio.uri, minio.access_key, minio.secret_key, and minio.bucket"
            );
        }

        Ok(())
    }
}

impl SrtConfig {
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

    fn validate(&self) -> Result<()> {
        let (start, end) = self.srt_port_range()?;
        if start >= end {
            anyhow::bail!(
                "Invalid SRT port range: start port {} must be less than end port {}",
                start,
                end
            );
        }

        Ok(())
    }
}

impl RtmpConfig {
    fn validate(&self) -> Result<()> {
        const MAX_RTMP_SESSION_TTL_SECS: u64 = 86_400;
        if self.session_ttl_secs > MAX_RTMP_SESSION_TTL_SECS {
            anyhow::bail!(
                "RTMP session TTL must be in 0..={} seconds, got {}",
                MAX_RTMP_SESSION_TTL_SECS,
                self.session_ttl_secs
            );
        }

        Ok(())
    }
}

impl PersistenceConfig {
    fn validate(&self) -> Result<()> {
        if self.duration <= 0 {
            anyhow::bail!("Segment duration must be positive");
        }

        let cache_dir = self.cache_dir.trim();
        if cache_dir == "." || cache_dir == ".." {
            anyhow::bail!("Persistence cache_dir cannot be '.' or '..'");
        }

        Ok(())
    }
}

impl QueueConfig {
    fn validate(&self) -> Result<()> {
        if self.rtmp_forward == 0
            || self.flv_relay == 0
            || self.packet_relay == 0
            || self.control == 0
            || self.event == 0
        {
            anyhow::bail!("All queue capacities must be greater than 0");
        }

        Ok(())
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
            app_name: default_rtmp_app_name(),
            session_ttl_secs: default_rtmp_session_ttl_secs(),
        }
    }
}

impl Default for HttpFlvConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: default_http_flv_port(),
        }
    }
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            duration: default_persistence_duration(),
            cache_dir: default_persistence_cache_dir(),
        }
    }
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            rtmp_forward: default_rtmp_forward_queue_capacity(),
            flv_relay: default_flv_relay_queue_capacity(),
            control: default_control_queue_capacity(),
            event: default_event_queue_capacity(),
            packet_relay: default_packet_relay_queue_capacity(),
        }
    }
}

pub fn load_config() -> &'static AppConfig {
    static SETTINGS: OnceLock<AppConfig> = OnceLock::new();

    SETTINGS.get_or_init(|| AppConfig::new().expect("Failed to load application settings"))
}
