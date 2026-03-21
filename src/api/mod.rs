pub mod grpc;

use std::sync::Arc;

use anyhow::Result;
use tracing::error;

use crate::api::grpc::contracts::{FlvPacketBus, MediaBus};
use crate::infra::{GrpcClientFactory, MinioClient, ShutdownManager};
use crate::ingest::StreamManager;
use crate::transport::RtmpServer;

use self::grpc::server::GrpcServer;

pub struct AppServer;

impl AppServer {
    pub async fn run() -> Result<()> {
        let config = crate::config::load_config();

        let (media_bus, flv_rx) = FlvPacketBus::new();
        let shutdown = ShutdownManager::new();

        let minio_config = config
            .minio
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Minio configuration not found"))?;
        let minio_client = MinioClient::create(minio_config).await?;

        let grpc_client_factory = GrpcClientFactory::new(config.grpc.callback.clone());

        let manager = Arc::new(StreamManager::new(
            config.ingest.clone(),
            config.egress.clone(),
            minio_client,
            grpc_client_factory,
            media_bus.sender(),
        ));

        let server_shutdown = shutdown.token();
        let server = RtmpServer::new(config.egress.clone(), manager.clone());

        tokio::spawn(async move {
            if let Err(e) = server.start(flv_rx, server_shutdown).await {
                error!(error = %e, "RTMP server failed");
            }
        });

        let grpc_shutdown = shutdown.token();
        let grpc_future = GrpcServer::new(manager, config.grpc.clone(), config.egress.clone())
            .serve(grpc_shutdown);

        // Spawn signal listener
        let shutdown_manager = shutdown.clone();
        tokio::spawn(async move {
            shutdown_manager.wait_for_signal().await;
        });

        let grpc_result = grpc_future.await;

        grpc_result
    }
}
