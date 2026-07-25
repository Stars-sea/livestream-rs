//! Transport configuration types.
//!
//! These are used by the transport servers (RTMP, HTTP-FLV, gRPC) and are
//! deserialized from `config.toml` by the binary crate.

use serde::Deserialize;

// ── RTMP ──

#[derive(Clone, Debug, Deserialize)]
pub struct RtmpConfig {
    #[serde(default = "default_rtmp_port")]
    pub port: u16,

    #[serde(default = "default_rtmp_app_name")]
    pub app_name: String,

    #[serde(default = "default_rtmp_session_ttl_secs")]
    pub session_ttl_secs: u64,
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

impl Default for RtmpConfig {
    fn default() -> Self {
        Self {
            port: default_rtmp_port(),
            app_name: default_rtmp_app_name(),
            session_ttl_secs: default_rtmp_session_ttl_secs(),
        }
    }
}

// ── gRPC ──

#[derive(Clone, Debug, Deserialize)]
pub struct GrpcConfig {
    #[serde(default = "default_grpc_port")]
    pub port: u16,
}

fn default_grpc_port() -> u16 {
    50051
}

impl Default for GrpcConfig {
    fn default() -> Self {
        Self {
            port: default_grpc_port(),
        }
    }
}

// ── HTTP-FLV ──

#[derive(Clone, Debug, Deserialize)]
pub struct HttpFlvConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_http_flv_port")]
    pub port: u16,
}

fn default_http_flv_port() -> u16 {
    8080
}

impl Default for HttpFlvConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: default_http_flv_port(),
        }
    }
}
