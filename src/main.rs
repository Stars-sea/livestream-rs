use anyhow::Result;
use crossfire::mpsc;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::pipeline::{PipeBus, UnifiedPipeFactory};
use crate::transport::TransportServer;

mod abstraction;
mod config;
mod dispatcher;
mod infra;
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

    infra::media::init();

    let config = config::load_config();

    let cancel_token = CancellationToken::new();
    let (rtmp_tag_tx, rtmp_tag_rx) = mpsc::unbounded_async();

    let factory = Arc::new(UnifiedPipeFactory::new(&config, rtmp_tag_tx).await?);
    let packet_bus = PipeBus::new();
    packet_bus.spawn_session_listener(factory);

    let transport_server = TransportServer::new(
        config.rtmp.clone(),
        config.srt.clone(),
        rtmp_tag_rx,
        packet_bus,
        cancel_token.child_token(),
    );

    let transport_handle = transport_server.spawn_task().await?;
    if let Err(e) = transport_handle.wait().await {
        error!("Error running transport server: {}", e);
    }

    if let Some(guard) = otel_guard {
        guard.shutdown();
    }

    Ok(())
}
