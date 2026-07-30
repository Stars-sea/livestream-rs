use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use livestream_test_utils::{Protocol, StreamConfig, StressConfig, run_stress_test};

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
    #[arg(long, required = true)]
    input_file: PathBuf,

    /// Protocol: rtmp or rtsp
    #[arg(long, default_value = "rtmp")]
    protocol: String,

    /// Output JSON report to stdout
    #[arg(long)]
    json: bool,
}
#[tokio::main]
#[allow(clippy::excessive_nesting)]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let protocol = match args.protocol.as_str() {
        "rtmp" => Protocol::Rtmp,
        "rtsp" => Protocol::Rtsp,
        other => {
            eprintln!("unknown protocol: {other} (use rtmp or rtsp)");
            std::process::exit(1);
        }
    };

    let duration = Duration::from_secs(args.duration);
    let parallel = args.parallel.unwrap_or(args.streams);

    let streams: Vec<StreamConfig> = (0..args.streams)
        .map(|i| StreamConfig {
            live_id: format!("stress-{i}"),
            protocol,
            input_file: args.input_file.clone(),
            duration,
        })
        .collect();

    let config = StressConfig {
        streams,
        parallel,
        grpc_addr: args.grpc_addr,
    };

    let report = run_stress_test(config).await;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    } else {
        println!("Stress test complete:");
        println!("  Total: {}", report.total_streams);
        println!("  Success: {}", report.successful);
        println!("  Failed: {}", report.failed);
        println!("  Duration: {:.1}s", report.total_duration_secs);
        let mut fail_count = 0usize;
        for r in &report.per_stream {
            if !r.success {
                fail_count += 1;
                println!("  [FAIL] {} errors: {:?}", r.live_id, r.errors);
            }
        }
        if fail_count == 0 {
            println!("  All {} streams succeeded.", report.total_streams);
        }
    }

    if report.failed > 0 {
        std::process::exit(1);
    }
}
