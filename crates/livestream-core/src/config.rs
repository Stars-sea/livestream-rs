//! Application configuration types.
//!
//! These structs are deserialized from `config.toml` by the binary crate.
//! Validation is embedded in each config type via `validate()` methods.

use anyhow::{Result, bail};
use serde::Deserialize;

// ── Segment (HLS) ──

/// Configuration for HLS segment production.
///
/// Used by HlsSegmenter to control segment duration, storage paths,
/// playlist size, and upload staging.
#[derive(Clone, Debug, Deserialize)]
pub struct SegmentConfig {
    /// Target duration of each TS segment in seconds.
    pub duration_secs: u64,

    /// Directory for temporary segment files before upload.
    pub cache_dir: String,

    /// Maximum number of segments to keep in the playlist (0 = unlimited).
    pub playlist_size: usize,

    /// Object key prefix in MinIO (e.g., "hls/{live_id}/").
    pub minio_prefix: String,

    /// Maximum number of staged-but-not-yet-uploaded segment files.
    /// When exceeded, the oldest staged file is evicted (LRU).
    /// Prevents disk exhaustion when MinIO is unreachable.
    pub max_staged_segments: usize,
}

impl Default for SegmentConfig {
    fn default() -> Self {
        Self {
            duration_secs: 10,
            cache_dir: String::new(),
            playlist_size: 5,
            minio_prefix: "hls".into(),
            max_staged_segments: 100,
        }
    }
}

// ── Transcode ──

/// Configuration for server-side media transcoding (MJPEG → H.264).
///
/// Only applies to RTSP sources that announce a MJPEG (RFC 2435) stream;
/// already-encoded H.264/AAC streams pass through untouched.
#[derive(Clone, Debug, Deserialize)]
pub struct TranscodeConfig {
    /// Target video bitrate in kbps.
    #[serde(default = "default_transcode_bitrate_kbps")]
    pub bitrate_kbps: u64,

    /// x264 preset name (only honored by the libx264 encoder).
    #[serde(default = "default_transcode_preset")]
    pub preset: String,

    /// Target keyframe interval in seconds.
    #[serde(default = "default_transcode_gop_secs")]
    pub gop_secs: f64,

    /// Output frame rate; `None` = follow the source frame rate.
    #[serde(default)]
    pub fps: Option<f64>,
}

impl Default for TranscodeConfig {
    fn default() -> Self {
        Self {
            bitrate_kbps: default_transcode_bitrate_kbps(),
            preset: default_transcode_preset(),
            gop_secs: default_transcode_gop_secs(),
            fps: None,
        }
    }
}

// ── Application ──

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

    #[serde(default)]
    pub transcode: TranscodeConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct TransportConfig {
    #[serde(default)]
    pub rtmp: RtmpConfig,

    #[serde(default)]
    pub rtsp: RtspConfig,
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
    pub segment: SegmentConfig,

    pub minio: Option<MinioConfig>,
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

    /// Maximum concurrent TCP connections (0 = unlimited).
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct HttpFlvConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_http_flv_port")]
    pub port: u16,

    /// Maximum concurrent HTTP-FLV playback connections (0 = unlimited).
    #[serde(default = "default_http_flv_max_connections")]
    pub max_connections: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RtspConfig {
    #[serde(default = "default_rtsp_port")]
    pub port: u16,

    #[serde(default = "default_rtsp_session_ttl_secs")]
    pub session_ttl_secs: u64,

    /// Maximum concurrent TCP connections (0 = unlimited).
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
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

// ── Defaults ──

fn default_grpc_port() -> u16 {
    50051
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

fn default_rtsp_port() -> u16 {
    8554
}

fn default_rtsp_session_ttl_secs() -> u64 {
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

fn default_max_connections() -> usize {
    1000
}

fn default_http_flv_max_connections() -> usize {
    2000
}

fn default_transcode_bitrate_kbps() -> u64 {
    1024
}

fn default_transcode_preset() -> String {
    "veryfast".to_string()
}

fn default_transcode_gop_secs() -> f64 {
    2.0
}

// ── Validation ──

impl AppConfig {
    pub fn new() -> Result<Self> {
        let mut builder = config::Config::builder()
            .add_source(config::File::new("config.toml", config::FileFormat::Toml).required(false))
            .add_source(
                config::Environment::default()
                    .try_parsing(true)
                    .separator("__"),
            );

        builder = builder
            .set_override_option("minio.uri", std::env::var("MINIO_URI").ok())?
            .set_override_option("minio.access_key", std::env::var("MINIO_ACCESSKEY").ok())?
            .set_override_option("minio.secret_key", std::env::var("MINIO_SECRETKEY").ok())?
            .set_override_option("minio.bucket", std::env::var("MINIO_BUCKET").ok())?;

        let config = builder
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build configuration: {}", e))?;

        let settings: AppConfig = config
            .try_deserialize()
            .map_err(|e| anyhow::anyhow!("Failed to deserialize configuration: {}", e))?;

        settings.validate()?;

        Ok(settings)
    }

    pub fn validate(&self) -> Result<()> {
        self.transport.validate()?;
        self.storage.validate()?;
        self.queue.validate()?;
        self.transcode.validate()?;
        Ok(())
    }
}

impl TranscodeConfig {
    fn validate(&self) -> Result<()> {
        if self.bitrate_kbps == 0 {
            bail!("transcode.bitrate_kbps must be greater than 0");
        }
        if self.gop_secs <= 0.0 {
            bail!("transcode.gop_secs must be greater than 0");
        }
        if self.preset.is_empty() {
            bail!("transcode.preset must not be empty");
        }
        if let Some(fps) = self.fps
            && fps <= 0.0
        {
            bail!("transcode.fps must be greater than 0 when set");
        }
        Ok(())
    }
}

impl StorageConfig {
    fn validate(&self) -> Result<()> {
        if self.segment.duration_secs == 0 {
            bail!("segment.duration_secs must be greater than 0");
        }
        let cache_dir = self.segment.cache_dir.trim();
        if cache_dir == "." || cache_dir == ".." {
            bail!("segment.cache_dir cannot be '.' or '..'");
        }
        // Note: an empty cache_dir is intentionally valid — SegmentWorkspace
        // falls back to the system temp directory in that case.
        if self.minio.is_none() {
            tracing::warn!("MinIO configuration is missing — HLS segment upload disabled");
        }
        Ok(())
    }
}

impl TransportConfig {
    fn validate(&self) -> Result<()> {
        self.rtmp.validate()?;
        self.rtsp.validate()?;
        Ok(())
    }
}

impl RtmpConfig {
    fn validate(&self) -> Result<()> {
        const MIN_RTMP_SESSION_TTL_SECS: u64 = 1;
        const MAX_RTMP_SESSION_TTL_SECS: u64 = 86_400;
        if !(MIN_RTMP_SESSION_TTL_SECS..=MAX_RTMP_SESSION_TTL_SECS).contains(&self.session_ttl_secs)
        {
            bail!(
                "RTMP session TTL must be in {}..={} seconds, got {}",
                MIN_RTMP_SESSION_TTL_SECS,
                MAX_RTMP_SESSION_TTL_SECS,
                self.session_ttl_secs
            );
        }
        if self.port == 0 {
            bail!("RTMP port must be non-zero");
        }

        Ok(())
    }
}

impl RtspConfig {
    fn validate(&self) -> Result<()> {
        const MIN_RTSP_SESSION_TTL_SECS: u64 = 1;
        const MAX_RTSP_SESSION_TTL_SECS: u64 = 86_400;
        if !(MIN_RTSP_SESSION_TTL_SECS..=MAX_RTSP_SESSION_TTL_SECS).contains(&self.session_ttl_secs)
        {
            bail!(
                "RTSP session TTL must be in {}..={} seconds, got {}",
                MIN_RTSP_SESSION_TTL_SECS,
                MAX_RTSP_SESSION_TTL_SECS,
                self.session_ttl_secs
            );
        }
        if self.port == 0 {
            bail!("RTSP port must be non-zero");
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
            bail!("All queue capacities must be greater than 0");
        }

        Ok(())
    }
}

// ── Default impls ──

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
            max_connections: default_max_connections(),
        }
    }
}

impl Default for HttpFlvConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: default_http_flv_port(),
            max_connections: default_http_flv_max_connections(),
        }
    }
}

impl Default for RtspConfig {
    fn default() -> Self {
        Self {
            port: default_rtsp_port(),
            session_ttl_secs: default_rtsp_session_ttl_secs(),
            max_connections: default_max_connections(),
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
