use anyhow::Result;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::pipeline::PipeBus;
use crate::transport::TransportServer;

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

    let transport_server = TransportServer::new(
        config.rtmp.clone(),
        config.srt.clone(),
        cancel_token.child_token(),
    );
    tokio::spawn(async move {
        if let Err(e) = transport_server.run().await {
            eprintln!("Error running transport server: {}", e);
        }
    });

    if let Some(guard) = otel_guard {
        guard.shutdown();
    }

    Ok(())
}
