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

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    info!("Starting LiveStream server");

    // core::set_log_level(Level::Trace);
    core::set_log_quiet();
    core::init();

    // Start RTMP server
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let server = services::RtmpServerFactory::default().create();
        server.start(rx).await
    });

    let minio_client = MinioClientFactory::default().create().await?;

    // Start GRPC & SRT pull stream server
    let stream_manager = Arc::new(StreamManager::new(minio_client, tx));
    let livestream = Arc::new(LivestreamService::new(stream_manager.clone()));

    GrpcServerFactory::default()
        .with_service(livestream)
        .with_manager(stream_manager)
        .serve()
        .await?;

    info!("Server shutdown complete");
    Ok(())
}
