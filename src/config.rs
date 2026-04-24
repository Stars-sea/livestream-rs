//! Application settings and configuration management.

use std::sync::OnceLock;

use anyhow::{Context, Result};
use config::{Config, Environment, File, FileFormat};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub srt: SrtConfig,

    #[serde(default)]
    pub grpc: GrpcConfig,

    #[serde(default)]
    pub rtmp: RtmpConfig,

    #[serde(default)]
    pub http_flv: HttpFlvConfig,

    #[serde(default)]
    pub persistence: PersistenceConfig,

    #[serde(default)]
    pub queue: QueueConfig,

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

    #[serde(default = "default_rtmp_appname")]
    pub appname: String,

    #[serde(default = "default_rtmp_ttl")]
    pub ttl: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct HttpFlvConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_http_flv_port")]
    pub port: u16,

    #[serde(default)]
    pub public_base_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PersistenceConfig {
    #[serde(default = "default_persistence_duration")]
    pub duration: i32,

    #[serde(default = "default_persistence_cachedir")]
    pub cachedir: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct QueueConfig {
    #[serde(default = "default_rtmp_forward_queue_capacity")]
    pub rtmpforward: usize,

    #[serde(default = "default_flv_relay_queue_capacity")]
    pub flvrelay: usize,

    #[serde(default = "default_packet_relay_queue_capacity")]
    pub packetrelay: usize,

    #[serde(default = "default_control_queue_capacity")]
    pub control: usize,

    #[serde(default = "default_event_queue_capacity")]
    pub event: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MinioConfig {
    pub uri: String,
    pub accesskey: String,
    pub secretkey: String,
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

impl Default for HttpFlvConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: default_http_flv_port(),
            public_base_url: None,
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

    pub fn validate(&self) -> Result<()> {
        self.validate_srt()?;
        self.validate_persistence()?;
        self.validate_rtmp()?;
        self.validate_queue()?;
        self.validate_minio()?;
        Ok(())
    }

    fn validate_srt(&self) -> Result<()> {
        let (start, end) = self.srt.srt_port_range()?;
        if start >= end {
            anyhow::bail!(
                "Invalid SRT port range: start port {} must be less than end port {}",
                start,
                end
            );
        }

        Ok(())
    }

    fn validate_persistence(&self) -> Result<()> {
        if self.persistence.duration <= 0 {
            anyhow::bail!("Segment duration must be positive");
        }

        let cache_dir = self.persistence.cachedir.trim();
        if cache_dir == "." || cache_dir == ".." {
            anyhow::bail!("Persistence cachedir cannot be '.' or '..'");
        }

        Ok(())
    }

    fn validate_rtmp(&self) -> Result<()> {
        const MAX_RTMP_SESSION_TTL_SECS: u64 = 86_400;
        if self.rtmp.ttl > MAX_RTMP_SESSION_TTL_SECS {
            anyhow::bail!(
                "RTMP session TTL must be in 0..={} seconds, got {}",
                MAX_RTMP_SESSION_TTL_SECS,
                self.rtmp.ttl
            );
        }

        Ok(())
    }

    fn validate_queue(&self) -> Result<()> {
        if self.queue.rtmpforward == 0
            || self.queue.flvrelay == 0
            || self.queue.packetrelay == 0
            || self.queue.control == 0
            || self.queue.event == 0
        {
            anyhow::bail!("All queue capacities must be greater than 0");
        }

        Ok(())
    }

    fn validate_minio(&self) -> Result<()> {
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

    SETTINGS.get_or_init(|| AppConfig::new().expect("Failed to load application settings"))
}
