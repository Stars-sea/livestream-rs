use std::sync::Arc;

use anyhow::Result;
use log::info;

use crate::ingest::{LivestreamService, StreamManager};
use crate::services::{GrpcServerFactory, MinioClientFactory};

mod core;
mod ingest;
mod rtmp_server;
mod services;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    info!("Starting LiveStream server");

    // core::set_log_level(Level::Trace);
    core::set_log_quiet();
    core::init();

    // Start RTMP server
    let mut rtmp_factory = services::RtmpServerFactory::default();
    let rtmp_server = rtmp_factory.create();
    let flv_packet_tx = rtmp_factory.get_flv_packet_sender();
    tokio::spawn(async move { rtmp_server.run().await });

    let minio_client = MinioClientFactory::default().create().await?;

    // Start GRPC & SRT pull stream server
    let stream_manager = Arc::new(StreamManager::new(minio_client, flv_packet_tx));
    let livestream = Arc::new(LivestreamService::new(stream_manager.clone()));

    GrpcServerFactory::default()
        .with_service(livestream)
        .with_manager(stream_manager)
        .serve()
        .await?;

    info!("Server shutdown complete");
    Ok(())
}
