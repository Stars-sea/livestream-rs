use std::sync::Arc;

use anyhow::Result;
use log::info;
use tokio::sync::mpsc;

use crate::ingest::{LivestreamService, StreamManager};
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

    // Start RTMP server
    let (tx, rx) = mpsc::unbounded_channel();

    let server = services::RtmpServerFactory::new()
        .with_port(settings.rtmp_port)
        .with_appname(settings.rtmp_app.clone())
        .create();
    tokio::spawn(async move { server.start(rx).await });

    let minio_client = MinioClientFactory::new(
        settings.minio_endpoint.clone(),
        settings.minio_access_key.clone(),
        settings.minio_secret_key.clone(),
        settings.minio_bucket.clone(),
    )
    .create()
    .await?;

    // Start GRPC & SRT pull stream server
    let stream_manager = Arc::new(StreamManager::new(settings.clone(), minio_client, tx));
    let livestream = Arc::new(LivestreamService::new(stream_manager.clone()));

    GrpcServerFactory::new(settings.grpc_port)
        .with_service(livestream)
        .with_manager(stream_manager)
        .serve()
        .await?;

    info!("Server shutdown complete");
    Ok(())
}
