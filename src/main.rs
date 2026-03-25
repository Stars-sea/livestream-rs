use std::{net::SocketAddr, str::FromStr};

use anyhow::Result;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::transport::rtmp::RtmpServer;

mod abstraction;
mod config;
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

    let addr = SocketAddr::from_str(&format!("0.0.0.0:{}", config.egress.port))?;
    let rtmp_server = RtmpServer::create(addr, config.egress.appname.clone(), cancel_token).await?;
    rtmp_server.run().await?;

    if let Some(guard) = otel_guard {
        guard.shutdown();
    }

    Ok(())
}
