pub mod grpc;

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::broadcast;
use tracing::error;

use crate::api::grpc::contracts::{FlvPacketBus, MediaBus};
use crate::infra::{GrpcClientFactory, MinioClient};
use crate::ingest::StreamManager;
use crate::transport::RtmpServer;

use self::grpc::server::GrpcServer;

pub struct AppServer;

impl AppServer {
    pub async fn run() -> Result<()> {
        let config = crate::config::load_config();

        let (media_bus, flv_rx) = FlvPacketBus::new();
        let (shutdown_tx, _) = broadcast::channel::<()>(1);

        let minio_config = config
            .minio
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Minio configuration not found"))?;
        let minio_client = MinioClient::create(minio_config).await?;

        let grpc_client_factory = GrpcClientFactory::new(config.grpc.callback.clone());

        let manager = Arc::new(StreamManager::new(
            minio_client,
            grpc_client_factory,
            media_bus.sender(),
        ));

        let server = RtmpServer::new(
            config.egress.clone(),
            manager.clone(),
            media_bus.sender(),
            manager.msg_tx(),
        );

        let shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            if let Err(e) = server.start(flv_rx, shutdown_rx).await {
                error!(error = %e, "RTMP server failed");
            }
        });

        let grpc_future =
            GrpcServer::new(manager, config.grpc.clone(), config.egress.clone()).serve();

        let grpc_result = grpc_future.await;

        let _ = shutdown_tx.send(());

        grpc_result
    }
}
