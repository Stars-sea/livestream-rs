use std::sync::Arc;

use anyhow::Result;
use log::{info, warn};
use tokio::signal;
use tonic::transport::Server;

use crate::ingest::{LivestreamServer, LivestreamService, StreamManager};

pub struct GrpcServerFactory {
    port: u16,
    livestream_service: Option<LivestreamService>,
    stream_manager: Option<Arc<StreamManager>>,
}

impl GrpcServerFactory {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            livestream_service: None,
            stream_manager: None,
        }
    }

    pub fn with_service(mut self, service: LivestreamService) -> Self {
        self.livestream_service = Some(service);
        self
    }

    pub fn with_manager(mut self, manager: Arc<StreamManager>) -> Self {
        self.stream_manager = Some(manager);
        self
    }

    pub async fn serve(self) -> Result<()> {
        let service = self
            .livestream_service
            .ok_or_else(|| anyhow::anyhow!("LivestreamService is required"))?;
        let manager = self
            .stream_manager
            .ok_or_else(|| anyhow::anyhow!("StreamManager is required"))?;

        let grpc_addr = format!("0.0.0.0:{}", self.port);
        info!("Server will listen on {}", grpc_addr);

        Server::builder()
            .add_service(LivestreamServer::new(service))
            .serve_with_shutdown(grpc_addr.parse()?, shutdown_signal(manager))
            .await?;

        Ok(())
    }
}

/// Handles graceful shutdown on SIGINT (Ctrl+C) or SIGTERM
async fn shutdown_signal(manager: Arc<StreamManager>) {
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

    let active_streams = manager.list_active_streams().await.unwrap_or_else(|e| {
        warn!("Failed to list active streams during shutdown: {}", e);
        vec![]
    });

    for live_id in active_streams {
        info!("Cleaning up stream: {}", live_id);
        manager.stop_stream(&live_id).await.unwrap_or_else(|e| {
            warn!("Failed to stop stream {}: {}", live_id, e);
        });
    }
}
