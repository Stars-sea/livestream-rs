//! Application settings and configuration management.

use anyhow::{Context, Result};
use std::env;

/// Application settings loaded from environment variables
#[derive(Clone, Debug)]
pub struct Settings {
    /// Host for SRT listeners (e.g., "srt.example.local")
    pub host: String,

    /// Port range for SRT listeners (format: "start-end", e.g., "4000-5000")
    pub srt_ports: String,
    /// gRPC Server Port
    pub grpc_port: u16,

    /// gRPC callback URL for stream events (e.g., "http://localhost:50051")
    pub grpc_callback: String,
    /// Segment duration in seconds for HLS/TS output
    pub segment_time: i32,
    /// Directory for temporary cache files
    pub cache_dir: String,

    /// RTMP Server Port
    pub rtmp_port: u16,
    /// RTMP Application Name
    pub rtmp_app: String,

    /// Minio Endpoint URL
    pub minio_endpoint: String,
    /// Minio Access Key
    pub minio_access_key: String,
    /// Minio Secret Key
    pub minio_secret_key: String,
    /// Minio Bucket Name
    pub minio_bucket: String,
}

impl Settings {
    /// Loads settings from environment variables.
    pub fn from_env() -> Result<Self> {
        let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        let srt_ports = env::var("SRT_PORTS").unwrap_or_else(|_| "30000-30010".to_string());
        let grpc_port = env::var("GRPC_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(50051);
        let grpc_callback =
            env::var("LIVE_SVC_GRPC").unwrap_or_else(|_| "http://localhost:50051".to_string());

        let segment_time = env::var("SEGMENT_TIME")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2); // Default 2s

        let cache_dir = env::var("CACHE_DIR").unwrap_or_else(|_| "./cache".to_string());

        let rtmp_port = env::var("RTMP_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1935);

        let rtmp_app = env::var("RTMP_APP").unwrap_or_else(|_| "lives".to_string());

        // Minio settings are required in production usually, but we can iterate or use defaults for dev
        let minio_endpoint =
            env::var("MINIO_URI").unwrap_or_else(|_| "http://localhost:9000".to_string());
        let minio_access_key =
            env::var("MINIO_ACCESSKEY").unwrap_or_else(|_| "minioadmin".to_string());
        let minio_secret_key =
            env::var("MINIO_SECRETKEY").unwrap_or_else(|_| "minioadmin".to_string());
        let minio_bucket = env::var("MINIO_BUCKET").unwrap_or_else(|_| "livestream".to_string());

        Ok(Self {
            host,
            srt_ports,
            grpc_port,
            grpc_callback,
            segment_time,
            cache_dir,
            rtmp_port,
            rtmp_app,
            minio_endpoint,
            minio_access_key,
            minio_secret_key,
            minio_bucket,
        })
    }

    /// Parses the SRT port range from the configuration.
    ///
    /// # Returns
    /// A tuple of (start_port, end_port)
    ///
    /// # Errors
    /// Returns an error if the port range format is invalid.
    pub fn srt_port_range(&self) -> Result<(u16, u16)> {
        let segments: Vec<u16> = self
            .srt_ports
            .split('-')
            .map(|s| {
                s.trim()
                    .parse::<u16>()
                    .with_context(|| format!("Invalid port number in range: '{}'", s))
            })
            .collect::<Result<Vec<u16>>>()?;

        if segments.len() != 2 {
            anyhow::bail!(
                "Invalid SRT port range format '{}': expected 'start-end'",
                self.srt_ports
            );
        }

        if segments[0] >= segments[1] {
            anyhow::bail!(
                "Invalid SRT port range '{}': start port must be less than end port",
                self.srt_ports
            );
        }

        Ok((segments[0], segments[1]))
    }
}
