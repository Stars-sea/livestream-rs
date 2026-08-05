use std::path::PathBuf;
use std::time::Duration;

use livestream_test_utils::{
    PortOverrides, Protocol, StreamConfig, StressConfig, env_or, run_stress_test,
};

/// Returns the path to the test video. Looks for `testdata/sample.mp4`
/// relative to the workspace root, or the `SAMPLE_VIDEO` env var.
fn test_input_path() -> PathBuf {
    // Try SAMPLE_VIDEO env var first
    if let Ok(p) = std::env::var("SAMPLE_VIDEO") {
        let path = PathBuf::from(&p);
        if path.exists() {
            return path;
        }
    }

    // Try relative to workspace root (tools/livestream-test-utils/../../testdata/sample.mp4)
    let candidates = [
        PathBuf::from("../../testdata/sample.mp4"),
        PathBuf::from("../testdata/sample.mp4"),
        PathBuf::from("testdata/sample.mp4"),
    ];
    for c in &candidates {
        if c.exists() {
            return c.clone();
        }
    }

    panic!("test video not found. Set SAMPLE_VIDEO env var or place sample.mp4 in testdata/");
}

fn grpc_addr_from_env() -> String {
    env_or("LIVESTREAM_GRPC_ADDR", "http://127.0.0.1:50051")
}

/// Integration test: push 3 concurrent RTMP streams and verify all succeed.
/// Requires the livestream server running on localhost.
#[tokio::test]
#[ignore = "requires running livestream server"]
async fn stress_3_streams_rtmp() {
    let config = StressConfig {
        streams: (0..3)
            .map(|i| StreamConfig {
                live_id: format!("stress-test-{i}"),
                protocol: Protocol::Rtmp,
                input_file: test_input_path(),
                duration: Duration::from_secs(10),
                port_overrides: PortOverrides::default(),
            })
            .collect(),
        parallel: 3,
        grpc_addr: grpc_addr_from_env(),
        minio: None,
    };
    let report = run_stress_test(config).await;
    assert_eq!(
        report.failed,
        0,
        "all streams must succeed, but {}/{} failed:\n{:#?}",
        report.failed,
        report.total_streams,
        report
            .per_stream
            .iter()
            .filter(|r| !r.success)
            .collect::<Vec<_>>()
    );
}
