use std::sync::Arc;

use anyhow::Result;
use log::info;
use tokio::sync::mpsc;

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
    env_logger::init();
    info!("Starting LiveStream server");

    let settings = Settings::from_env()?;

    // core::set_log_level(Level::Trace);
    core::set_log_quiet();
    core::init();

    let (tx, rx) = mpsc::unbounded_channel();

    let minio_client = MinioClientFactory::new(
        settings.minio_endpoint.clone(),
        settings.minio_access_key.clone(),
        settings.minio_secret_key.clone(),
        settings.minio_bucket.clone(),
    )
    .create()
    .await?;
    let manager = Arc::new(StreamManager::new(settings.clone(), minio_client, tx));

    // Start RTMP server
    let server = RtmpServer::new(settings.rtmp_port, settings.rtmp_app, manager.clone());
    tokio::spawn(async move { server.start(rx).await });

    // Start GRPC & SRT pull stream server
    GrpcServerFactory::new(settings.grpc_port)
        .with_service(LivestreamService::new(manager.clone()))
        .with_manager(manager)
        .serve()
        .await?;

    info!("Server shutdown complete");
    Ok(())
}
