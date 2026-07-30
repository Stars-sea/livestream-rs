//! Stress test types and concurrent runner.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::time::sleep;

use crate::client::{connect_and_get_info, stop_livestream, verify_connected, ServicePorts};
use crate::primitives::{kill_and_wait, pull_and_verify, spawn_push};
use crate::proto::{StartLivestreamRequest, livestream_client::LivestreamClient};

// ── types ──

/// Transport protocol for stress test streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    Rtmp,
    Rtsp,
}

impl Protocol {
    /// gRPC `InputProtocol` discriminant for StartLivestream.
    fn input_protocol_i32(self) -> i32 {
        match self {
            Protocol::Rtmp => crate::proto::InputProtocol::Rtmp as i32,
            Protocol::Rtsp => crate::proto::InputProtocol::Rtsp as i32,
        }
    }

    /// FFmpeg format arguments for the push command.
    fn format_args(self) -> &'static [&'static str] {
        match self {
            Protocol::Rtmp => &["-c", "copy", "-f", "flv"],
            Protocol::Rtsp => &["-c", "copy", "-f", "rtsp"],
        }
    }

    /// Pull URL and label for verification playback.
    fn pull_url(self, ports: &ServicePorts, live_id: &str) -> (String, String) {
        match self {
            Protocol::Rtmp => {
                let url = format!("rtmp://localhost:{}/lives/{}", ports.rtmp, live_id);
                let label = format!("rtmp:{}", live_id);
                (url, label)
            }
            Protocol::Rtsp => {
                let url = format!("rtsp://localhost:{}/{}", ports.rtsp, live_id);
                let label = format!("rtsp:{}", live_id);
                (url, label)
            }
        }
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Protocol::Rtmp => write!(f, "rtmp"),
            Protocol::Rtsp => write!(f, "rtsp"),
        }
    }
}

/// Configuration for a single stress test stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamConfig {
    pub live_id: String,
    pub protocol: Protocol,
    pub input_file: PathBuf,
    pub duration: Duration,
}

/// Result of a single stream test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamResult {
    pub live_id: String,
    pub success: bool,
    pub push_latency_ms: u64,
    pub pull_frames_detected: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// Overall stress test configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StressConfig {
    pub streams: Vec<StreamConfig>,
    pub parallel: usize,
    pub grpc_addr: String,
}

/// Stress test report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StressReport {
    pub total_streams: usize,
    pub successful: usize,
    pub failed: usize,
    pub total_duration_secs: f64,
    pub per_stream: Vec<StreamResult>,
}

// ── runner ──

/// Run a single stream end-to-end: StartLivestream → push → verify → pull → StopLivestream.
pub async fn run_single_stream(
    client: &mut LivestreamClient<tonic::transport::Channel>,
    ports: &ServicePorts,
    config: &StreamConfig,
) -> StreamResult {
    let t0 = Instant::now();
    let mut errors: Vec<String> = vec![];

    let start_resp = match client
        .start_livestream(StartLivestreamRequest {
            live_id: config.live_id.clone(),
            passphrase: None,
            input_protocol: config.protocol.input_protocol_i32(),
        })
        .await
    {
        Ok(r) => r.into_inner(),
        Err(e) => {
            return stream_error(config, 0, false, &[format!("StartLivestream failed: {e}")]);
        }
    };

    let push_url = build_push_url(&start_resp, ports, config, &mut errors);

    let mut push = match spawn_push(&config.input_file, config.protocol.format_args(), &push_url) {
        Ok(p) => p,
        Err(e) => {
            errors.push(format!("push spawn failed: {e}"));
            let _ = stop_livestream(client, &config.live_id).await;
            return stream_error(config, t0.elapsed().as_millis() as u64, false, &errors);
        }
    };

    sleep(Duration::from_secs(3)).await;
    let push_latency = t0.elapsed().as_millis() as u64;

    if let Err(e) = verify_connected(client, &config.live_id).await {
        errors.push(format!("verify_connected: {e}"));
    }

    let pull_frames = run_pull_verify(config, ports, &mut errors).await;

    kill_and_wait(&mut push);
    stop_livestream(client, &config.live_id).await;

    StreamResult {
        live_id: config.live_id.clone(),
        success: errors.is_empty(),
        push_latency_ms: push_latency,
        pull_frames_detected: pull_frames,
        errors,
    }
}

fn stream_error(config: &StreamConfig, latency_ms: u64, frames: bool, errors: &[String]) -> StreamResult {
    StreamResult {
        live_id: config.live_id.clone(),
        success: false,
        push_latency_ms: latency_ms,
        pull_frames_detected: frames,
        errors: errors.to_vec(),
    }
}

fn build_push_url(
    start_resp: &crate::proto::StartLivestreamResponse,
    ports: &ServicePorts,
    config: &StreamConfig,
    errors: &mut Vec<String>,
) -> String {
    let endpoints = start_resp
        .descriptor
        .as_ref()
        .and_then(|d| d.endpoints.as_ref());
    let ingest = endpoints.and_then(|e| e.ingest.as_ref());

    match config.protocol {
        Protocol::Rtmp => ingest
            .and_then(|i| i.rtmp.as_ref())
            .map(|e| format!("rtmp://localhost:{}/{}/{}", e.port, e.app_name, e.stream_key))
            .unwrap_or_else(|| {
                errors.push("no RTMP ingest endpoint".into());
                format!("rtmp://localhost:{}/lives/{}", ports.rtmp, config.live_id)
            }),
        Protocol::Rtsp => ingest
            .and_then(|i| i.rtsp.as_ref())
            .map(|e| format!("rtsp://localhost:{}/{}", e.port, e.path.trim_start_matches('/')))
            .unwrap_or_else(|| {
                errors.push("no RTSP ingest endpoint".into());
                format!("rtsp://localhost:{}/{}", ports.rtsp, config.live_id)
            }),
    }
}

async fn run_pull_verify(
    config: &StreamConfig,
    ports: &ServicePorts,
    errors: &mut Vec<String>,
) -> bool {
    let (url, label) = config.protocol.pull_url(ports, &config.live_id);
    match pull_and_verify(&url, &label, config.duration).await {
        Ok(()) => true,
        Err(e) => {
            errors.push(format!("{} pull: {e}", config.protocol));
            false
        }
    }
}

/// Run N streams concurrently, bounded by `parallel` semaphore.
pub async fn run_stress_test(config: StressConfig) -> StressReport {
    let t0 = Instant::now();
    let sem = Arc::new(tokio::sync::Semaphore::new(config.parallel));

    let mut handles = Vec::with_capacity(config.streams.len());
    for stream_cfg in &config.streams {
        let permit = sem.clone().acquire_owned().await;
        let stream_cfg = stream_cfg.clone();
        let grpc_addr = config.grpc_addr.clone();

        handles.push(tokio::spawn(async move {
            let _permit = permit;
            let (mut client, ports) = match connect_and_get_info(&grpc_addr).await {
                Ok(c) => c,
                Err(e) => {
                    return StreamResult {
                        live_id: stream_cfg.live_id.clone(),
                        success: false,
                        push_latency_ms: 0,
                        pull_frames_detected: false,
                        errors: vec![format!("connect failed: {e}")],
                    };
                }
            };
            run_single_stream(&mut client, &ports, &stream_cfg).await
        }));
    }

    let mut per_stream = Vec::with_capacity(handles.len());
    for h in handles {
        match h.await {
            Ok(r) => per_stream.push(r),
            Err(e) => {
                per_stream.push(StreamResult {
                    live_id: "unknown".into(),
                    success: false,
                    push_latency_ms: 0,
                    pull_frames_detected: false,
                    errors: vec![format!("task panic: {e}")],
                });
            }
        }
    }

    let successful = per_stream.iter().filter(|r| r.success).count();
    StressReport {
        total_streams: per_stream.len(),
        successful,
        failed: per_stream.len() - successful,
        total_duration_secs: t0.elapsed().as_secs_f64(),
        per_stream,
    }
}
