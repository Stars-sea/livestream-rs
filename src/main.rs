use std::env::var;
use std::sync::Arc;

use anyhow::Result;
use log::info;
use tokio::signal;
use tonic::transport::Server;

use crate::livestream::events::{OnSegmentComplete, SegmentCompleteStream, SegmentCompleteTx};
use crate::livestream::service::{LiveStreamService, LivestreamServer};
use crate::persistence::minio::MinioClient;
use crate::persistence::redis::RedisClient;

mod core;
mod livestream;
mod persistence;
mod settings;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    info!("Starting LiveStream server");

    // core::set_log_level(Level::Trace);
    core::set_log_quiet();
    core::init();

    let settings = settings::Settings::load()?;

    let redis_client = RedisClient::create(&env_var("REDIS_URI")?).await?;

    let segment_complete_tx = spawn_minio_uploader().await?;

    let livestream = Arc::new(LiveStreamService::new(
        redis_client,
        segment_complete_tx,
        settings,
    ));

    let grpc_port = env_var("GRPC_PORT")?;
    let grpc_addr = format!("0.0.0.0:{}", grpc_port);
    info!("Server will listen on {}", grpc_addr);

    Server::builder()
        .add_service(LivestreamServer::new(livestream))
        .serve_with_shutdown(grpc_addr.parse()?, shutdown_signal())
        .await?;

    info!("Server shutdown complete");
    Ok(())
}

fn env_var(key: &str) -> Result<String> {
    var(key).map_err(|_| anyhow::anyhow!("{} environment variable not set", key))
}

async fn spawn_minio_uploader() -> Result<SegmentCompleteTx> {
    let minio_client = MinioClient::create(
        &env_var("MINIO_URI")?,
        &env_var("MINIO_ACCESSKEY")?,
        &env_var("MINIO_SECRETKEY")?,
        &env_var("MINIO_BUCKET")?,
    )
    .await?;

    let (tx, rx) = OnSegmentComplete::channel();
    tokio::spawn(persistence::minio::minio_uploader(
        SegmentCompleteStream::new(rx),
        minio_client,
    ));

    Ok(tx)
}

/// Handles graceful shutdown on SIGINT (Ctrl+C) or SIGTERM
async fn shutdown_signal() {
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

    // TODO: Notify tasks to shut down gracefully
    tokio::select! {
        _ = ctrl_c => {
            info!("Received Ctrl+C signal, shutting down gracefully...");
        },
        _ = terminate => {
            info!("Received SIGTERM signal, shutting down gracefully...");
        },
    }
}
