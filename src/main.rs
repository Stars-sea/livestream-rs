use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::info;

use crate::ingest::{LivestreamService, StreamManager};
use crate::publish::RtmpServer;
use crate::services::{GrpcServerFactory, MinioClientFactory};

mod core;
mod ingest;
mod publish;
mod services;
mod settings;

pub use settings::Settings;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .init();

    info!("Starting LiveStream server");

    let settings = Settings::new()?;

    // core::set_log_level(Level::Trace);
    core::set_log_quiet();
    core::init();

    let (tx, rx) = mpsc::unbounded_channel();
    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);

    let minio_client = MinioClientFactory::new(
        settings.minio_uri.clone(),
        settings.minio_accesskey.clone(),
        settings.minio_secretkey.clone(),
        settings.minio_bucket.clone(),
    )
    .create()
    .await?;
    let manager = Arc::new(StreamManager::new(settings.clone(), minio_client, tx));

    // Start RTMP server
    let server = RtmpServer::new(settings.rtmp_port, settings.rtmp_app, manager.clone());
    let shutdown_rx = shutdown_tx.subscribe();
    tokio::spawn(async move {
        if let Err(e) = server.start(rx, shutdown_rx).await {
            tracing::error!("RTMP server failed: {}", e);
        }
    });

    // Start GRPC & SRT pull stream server
    let grpc_future = GrpcServerFactory::new(settings.grpc_port)
        .with_service(LivestreamService::new(manager.clone()))
        .with_manager(manager)
        .serve();

    // Wait for gRPC server (which listens for Ctrl+C)
    if let Err(e) = grpc_future.await {
        tracing::error!("gRPC server error: {}", e);
    }

    // Signal other components to shutdown
    let _ = shutdown_tx.send(());
    Ok(())
}
