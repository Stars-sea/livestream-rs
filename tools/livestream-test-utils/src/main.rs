use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use livestream_test_utils::{PortOverrides, Protocol, StreamConfig, StressConfig, run_stress_test};

#[derive(Parser, Debug)]
#[command(
    name = "livestream-stress",
    about = "Concurrent stress test for livestream service"
)]
struct Args {
    /// Number of concurrent streams to test
    #[arg(long, default_value = "10")]
    streams: usize,

    /// Stream duration in seconds
    #[arg(long, default_value = "30")]
    duration: u64,

    /// Maximum parallel streams
    #[arg(long)]
    parallel: Option<usize>,

    /// gRPC address of the livestream service
    #[arg(long, default_value = "http://127.0.0.1:50051")]
    grpc_addr: String,

    /// Input video file for push
    #[arg(long, required = false)]
    input_file: Option<PathBuf>,

    /// Protocol: rtmp or rtsp
    #[arg(long, default_value = "rtmp")]
    protocol: String,

    /// Override the RTMP port reported by GetServiceInfo (host-reachable port).
    #[arg(long)]
    rtmp_port: Option<u16>,

    /// Override the RTSP port reported by GetServiceInfo (host-reachable port).
    #[arg(long)]
    rtsp_port: Option<u16>,

    /// Override the HTTP-FLV port reported by GetServiceInfo (host-reachable port).
    #[arg(long)]
    http_flv_port: Option<u16>,

    /// Base live_id for the created streams. Stream 0 gets it verbatim,
    /// subsequent streams get "-{i}" suffixes.
    #[arg(long)]
    live_id: Option<String>,

    /// Only precreate sessions (StartLivestream), then exit — no push or
    /// verification. Used by e2e scripts that push with external tools.
    #[arg(long)]
    precreate_only: bool,

    /// Output JSON report to stdout
    #[arg(long)]
    json: bool,

    /// MinIO connection string (Endpoint=...;AccessKey=...;SecretKey=...).
    /// When set, HLS persistence is verified for every stream.
    #[arg(long)]
    minio_connection_string: Option<String>,

    /// MinIO bucket for HLS objects (used when the connection string has no Bucket key).
    #[arg(long, default_value = "videos")]
    minio_bucket: String,
}

/// live_id for stream `i`: base verbatim for stream 0, "{base}-{i}" otherwise,
/// "stress-{i}" when no base is given.
fn stream_live_id(base: Option<&str>, i: usize) -> String {
    match base {
        Some(b) if i == 0 => b.to_string(),
        Some(b) => format!("{b}-{i}"),
        None => format!("stress-{i}"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        // 日志走 stderr，保证 stdout 只含 JSON 报告（--json 模式依赖）。
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    let protocol = match args.protocol.as_str() {
        "rtmp" => Protocol::Rtmp,
        "rtsp" => Protocol::Rtsp,
        other => bail!("unknown protocol: {other} (use rtmp or rtsp)"),
    };
    let duration = Duration::from_secs(args.duration);
    let parallel = args.parallel.unwrap_or(args.streams);

    let input_file = match &args.input_file {
        Some(p) => p.clone(),
        None if args.precreate_only => PathBuf::new(),
        None => bail!("--input-file is required unless --precreate-only is set"),
    };

    let minio = match &args.minio_connection_string {
        Some(conn) => Some(
            livestream_test_utils::parse_connection_string(conn, &args.minio_bucket)
                .context("invalid --minio-connection-string")?,
        ),
        None => None,
    };

    let port_overrides = PortOverrides {
        rtmp: args.rtmp_port,
        rtsp: args.rtsp_port,
        http_flv: args.http_flv_port,
    };
    let streams: Vec<StreamConfig> = (0..args.streams)
        .map(|i| StreamConfig {
            live_id: stream_live_id(args.live_id.as_deref(), i),
            protocol,
            input_file: input_file.clone(),
            duration,
            port_overrides,
        })
        .collect();

    let config = StressConfig {
        streams,
        parallel,
        grpc_addr: args.grpc_addr,
        minio,
    };

    if args.precreate_only {
        let failed = livestream_test_utils::precreate_streams(&config).await;
        ensure!(failed == 0, "precreate failed for {failed} stream(s)");
        return Ok(());
    }

    let report = run_stress_test(config).await;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("serialize stress report")?
        );
    } else {
        println!("Stress test complete:");
        println!("  Total: {}", report.total_streams);
        println!("  Success: {}", report.successful);
        println!("  Failed: {}", report.failed);
        println!("  Duration: {:.1}s", report.total_duration_secs);
        for r in report.per_stream.iter().filter(|r| !r.success) {
            println!("  [FAIL] {} errors: {:?}", r.live_id, r.errors);
        }
        if report.failed == 0 {
            println!("  All {} streams succeeded.", report.total_streams);
        }
    }

    if report.failed > 0 {
        bail!(
            "stress test failed: {}/{} streams",
            report.failed,
            report.total_streams
        );
    }
    Ok(())
}
