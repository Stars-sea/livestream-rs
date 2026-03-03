pub mod contracts;
mod grpc;
mod rtmp;

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{broadcast, mpsc};
use tracing::error;

use crate::ingest::StreamManager;
use crate::ingest::events::StreamMessage;
use crate::server::contracts::{FlvPacketBus, MediaBus};
use crate::services::MinioClient;

use self::grpc::GrpcServer;
use self::rtmp::RtmpServer;

pub struct AppServer;

impl AppServer {
    pub async fn run() -> Result<()> {
        let (media_bus, flv_rx) = FlvPacketBus::new();
        let (stream_msg_tx, stream_msg_rx) = mpsc::unbounded_channel::<(StreamMessage, tracing::Span)>();
        let (shutdown_tx, _) = broadcast::channel::<()>(1);

        let minio_client = MinioClient::create_default().await?;
        let manager = Arc::new(StreamManager::new(
            minio_client,
            media_bus.sender(),
            stream_msg_tx.clone(),
            stream_msg_rx,
        ));

        let server = RtmpServer::new(
            manager.clone(),
            media_bus.sender(),
            stream_msg_tx,
        );

        let shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            if let Err(e) = server.start(flv_rx, shutdown_rx).await {
                error!(error = %e, "RTMP server failed");
            }
        });

        let grpc_future = GrpcServer::new(manager).serve();

        let grpc_result = grpc_future.await;

        let _ = shutdown_tx.send(());

        grpc_result
    }
}
