use std::{net::SocketAddr, str::FromStr};

use anyhow::Result;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::config::RtmpConfig;
use crate::pipeline::PipeBus;
use crate::transport::rtmp::RtmpServer;

mod abstraction;
mod config;
mod infra;
mod media;
mod pipeline;
mod telemetry;
mod transport;

#[tokio::main]
async fn main() -> Result<()> {
    let otel_guard = telemetry::setup_telemetry()?;

    info!(
        version = env!("CARGO_PKG_VERSION"),
        "Starting LiveStream server"
    );

    media::init();

    let config = config::load_config();

    let cancel_token = CancellationToken::new();

    let packet_bus = PipeBus::new();

    tokio::spawn(run_rtmp_server(&config.rtmp, cancel_token.child_token()));

    if let Some(guard) = otel_guard {
        guard.shutdown();
    }

    Ok(())
}

async fn run_rtmp_server(egress: &RtmpConfig, cancel_token: CancellationToken) -> Result<()> {
    let addr = SocketAddr::from_str(&format!("0.0.0.0:{}", egress.port))?;
    let server = RtmpServer::create(addr, egress.appname.clone(), cancel_token).await?;
    server.run().await
}
