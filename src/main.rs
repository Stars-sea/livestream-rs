use std::{net::SocketAddr, str::FromStr};

use anyhow::Result;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::transport::rtmp::RtmpServer;

mod abstraction;
mod config;
mod media;
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

    // if let Err(e) = AppServer::run().await {
    //     error!(error = %e, "Server runtime error");
    // }

    if let Some(guard) = otel_guard {
        guard.shutdown();
    }

    Ok(())
}
