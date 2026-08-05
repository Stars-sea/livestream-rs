//! livestream-test-utils — reusable test primitives and stress test runner.
//!
//! Provides gRPC helpers, stream push/pull/verify primitives, and a concurrent
//! stress test runner.  Usable from both integration tests (`#[tokio::test]`)
//! and the standalone CLI binary (`main.rs`).

mod proto {
    #[allow(clippy::excessive_nesting)]
    mod inner {
        tonic::include_proto!("livestream");
    }
    pub use inner::*;
}

mod client;
mod minio;
mod primitives;
mod runner;

pub use client::{
    PortOverrides, ServicePorts, connect_and_get_info, stop_livestream, verify_connected,
};
pub use minio::{HlsVerification, MinioConfig, parse_connection_string, verify_hls};
pub use primitives::{env_or, kill_and_wait, pull_and_verify, spawn_push};
pub use runner::{
    Protocol, StreamConfig, StreamResult, StressConfig, StressReport, precreate_streams,
    run_single_stream, run_stress_test,
};

pub use proto::livestream_client::LivestreamClient;
