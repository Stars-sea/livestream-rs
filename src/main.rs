use anyhow::Result;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::pipeline::middleware::BroadcastMiddleware;
use crate::pipeline::middleware::SegmentMiddleware;
use crate::pipeline::{PipeBus, PipeFactory, UnifiedPacketContext, UnifiedPipeFactory};
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

    let broadcast = Arc::new(BroadcastMiddleware::<UnifiedPacketContext>::new());
    let segment = Arc::new(SegmentMiddleware::new());
    let packet_pipe = Arc::new(UnifiedPipeFactory::new(broadcast, segment).create());
    let _packet_bus = PipeBus::new(packet_pipe);

    let transport_server = TransportServer::new(
        config.rtmp.clone(),
        config.srt.clone(),
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
