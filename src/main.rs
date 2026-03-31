use anyhow::Result;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::pipeline::{PipeBus, PipeFactory, UnifiedPipeFactory};
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

    let minio_client = infra::MinioClient::create(config.minio.clone().unwrap()).await?;
    let packet_pipe = Arc::new(UnifiedPipeFactory::new(minio_client).create());
    let packet_bus = PipeBus::new(packet_pipe);

    let transport_server = TransportServer::new(
        config.rtmp.clone(),
        config.srt.clone(),
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
