use std::env::var;
use std::sync::Arc;

use anyhow::Result;
use log::{info, warn};
use tokio::signal;
use tonic::transport::Server;

use crate::livestream::{LiveStreamService, LivestreamServer};
use crate::services::MinioClient;

mod core;
mod livestream;
mod services;
mod settings;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    info!("Starting LiveStream server");

    // core::set_log_level(Level::Trace);
    core::set_log_quiet();
    core::init();

    let settings = settings::Settings::load()?;

    let minio_client = MinioClient::create(
        &env_var("MINIO_URI")?,
        &env_var("MINIO_ACCESSKEY")?,
        &env_var("MINIO_SECRETKEY")?,
        &env_var("MINIO_BUCKET")?,
    )
    .await?;

    let livestream = Arc::new(LiveStreamService::new(minio_client, settings));

    let grpc_port = env_var("GRPC_PORT")?;
    let grpc_addr = format!("0.0.0.0:{}", grpc_port);
    info!("Server will listen on {}", grpc_addr);

    Server::builder()
        .add_service(LivestreamServer::new(livestream.clone()))
        .serve_with_shutdown(grpc_addr.parse()?, shutdown_signal(livestream))
        .await?;

    info!("Server shutdown complete");
    Ok(())
}

fn env_var(key: &str) -> Result<String> {
    var(key).map_err(|_| anyhow::anyhow!("{} environment variable not set", key))
}

/// Handles graceful shutdown on SIGINT (Ctrl+C) or SIGTERM
async fn shutdown_signal(server: Arc<LiveStreamService>) {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            info!("Received Ctrl+C signal, shutting down gracefully...");
        },
        _ = terminate => {
            info!("Received SIGTERM signal, shutting down gracefully...");
        },
    }

    let active_streams = server.list_active_streams_impl().await.unwrap_or_else(|e| {
        warn!("Failed to list active streams during shutdown: {}", e);
        vec![]
    });

    for live_id in active_streams {
        info!("Cleaning up stream: {}", live_id);
        server.stop_stream_impl(&live_id).await.unwrap_or_else(|e| {
            warn!("Failed to stop stream {}: {}", live_id, e);
        });
    }
}
