//! Application settings and configuration management.

use anyhow::{Context, Result};
use log::warn;
use std::{fs, path::Path};

/// Application settings loaded from settings.json
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct Settings {
    /// Host for SRT listeners (e.g., "srt.example.local")
    pub host: String,

    /// Port range for SRT listeners (format: "start-end", e.g., "4000-5000")
    pub srt_ports: String,
    /// Port for RTMP streaming output (e.g., 1935)
    pub rtmp_port: u16,

    /// gRPC callback URL for stream events (e.g., "http://localhost:50051")
    pub grpc_callback: String,
    /// Segment duration in seconds for HLS/TS output
    pub segment_time: i32,
    /// Directory for temporary cache files
    pub cache_dir: String,
}

impl Settings {
    const DEFAULT_PATH: &str = "./settings.json";

    /// Loads settings from the default settings.json file.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read or parsed.
    pub fn load() -> Result<Self> {
        let path = Path::new(Self::DEFAULT_PATH);

        let data = fs::read_to_string(path)?;
        let mut settings: Settings = serde_json::from_str(&data)
            .with_context(|| format!("Failed to parse settings from {}", path.display()))?;

        if let Ok(host) = std::env::var("HOST") {
            settings.host = host;
        }

        if let Ok(srt_ports) = std::env::var("SRT_PORTS") {
            settings.srt_ports = srt_ports;
        }

        if let Ok(rtmp_port) = std::env::var("RTMP_PORT") {
            if let Ok(port) = rtmp_port.parse::<u16>() {
                settings.rtmp_port = port;
            } else {
                warn!(
                    "Invalid RTMP_PORT environment variable '{}', using default {}",
                    rtmp_port, settings.rtmp_port
                );
            }
        }

        if let Ok(grpc_callback) = std::env::var("LIVE_SVC_GRPC") {
            settings.grpc_callback = grpc_callback;
        }

        if let Ok(segment_time_str) = std::env::var("SEGMENT_TIME") {
            if let Ok(segment_time) = segment_time_str.parse::<i32>() {
                settings.segment_time = segment_time;
            } else {
                warn!(
                    "Invalid SEGMENT_TIME environment variable '{}', using default {}",
                    segment_time_str, settings.segment_time
                );
            }
        }

        Ok(settings)
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
