use anyhow::Result;
use tracing::{error, info};

use crate::api::AppServer;

mod api;
mod config;
mod egress;
mod infra;
mod ingest;
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

    if let Err(e) = AppServer::run().await {
        error!(error = %e, "Server runtime error");
    }

    if let Some(guard) = otel_guard {
        guard.shutdown();
    }

    Ok(())
}
