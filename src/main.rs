//! Makerverse livestream server.
//!
//! Coordinates RTMP ingest, HTTP-FLV playback, gRPC control plane,
//! and MinIO persistence.

mod config;
mod infra;

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use infra::persistence::PersistenceClient;
use livestream_core::channel;
use livestream_pipeline::factory;
use livestream_pipeline::sink::minio::ObjectUploader;
use livestream_transport::{
    controller::TransportController, dispatcher::EventDispatcher, flv::FlvEgressHub,
    grpc::GrpcServer, http_flv::HttpFlvServer, registry::SessionRegistry, rtmp::RtmpServer,
    rtsp::server::RtspServer,
};

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Initialize FFmpeg and telemetry
    livestream_media::init();
    let _guard = livestream_telemetry::setup_telemetry()?;

    // 2. Load configuration
    let config = config::load_config();

    // 3. Create MinIO persistence client (or null uploader for dev/test)
    let minio: Arc<dyn ObjectUploader> = match &config.storage.minio {
        Some(minio_cfg) => {
            let client = PersistenceClient::create(minio_cfg.clone())
                .await
                .expect("Failed to create MinIO persistence client");
            Arc::new(client) as Arc<dyn ObjectUploader>
        }
        None => factory::null_uploader(),
    };
    let segment_cfg = config.storage.segment.clone();

    // 4. Create shared infrastructure
    let cancel = CancellationToken::new();
    let flv_egress_hub = Arc::new(FlvEgressHub::new());
    let registry = Arc::new(SessionRegistry::new());
    let dispatcher = Arc::new(EventDispatcher::new());

    // 5. Create control channel and RTMP server
    let (rtmp_tx, rtmp_rx) = channel::mpsc("ctrl_rtmp", None, config.queue.control);
    let rtmp_addr = SocketAddr::from_str(&format!("0.0.0.0:{}", config.transport.rtmp.port))?;
    let session_ttl = Duration::from_secs(config.transport.rtmp.session_ttl_secs);
    let rtmp_server = RtmpServer::create(
        rtmp_addr,
        config.transport.rtmp.app_name.clone(),
        session_ttl,
        rtmp_rx,
        flv_egress_hub.clone(),
        minio.clone(),
        segment_cfg.clone(),
        cancel.child_token(),
        registry.clone(),
        dispatcher.clone(),
    )
    .await?;

    // 6. Create RTSP control channel and server
    let (rtsp_tx, rtsp_rx) = channel::mpsc("ctrl_rtsp", None, config.queue.control);
    let rtsp_addr = SocketAddr::from_str(&format!("0.0.0.0:{}", config.transport.rtsp.port))?;
    let rtsp_ttl = Duration::from_secs(config.transport.rtsp.session_ttl_secs);

    let rtsp_server = RtspServer::create(
        rtsp_addr,
        rtsp_rx,
        flv_egress_hub.clone(),
        registry.clone(),
        dispatcher.clone(),
        rtsp_ttl,
        minio.clone(),
        segment_cfg.clone(),
        cancel.child_token(),
    )
    .await?;

    let controller = Arc::new(TransportController::new(registry.clone(), rtmp_tx, rtsp_tx));

    // 8. Create gRPC server
    let grpc_server = GrpcServer::new(
        config.services.grpc.port,
        config.transport.rtmp.port,
        config.transport.rtmp.app_name.clone(),
        config.transport.rtsp.port,
        config.services.http_flv.enabled,
        config.services.http_flv.port,
        controller,
        registry.clone(),
        dispatcher.clone(),
    );

    // 9. Create HTTP-FLV server (optional)
    let http_flv_server = if config.services.http_flv.enabled {
        Some(
            HttpFlvServer::create(
                config.services.http_flv.port,
                flv_egress_hub.clone(),
                registry.clone(),
                cancel.child_token(),
            )
            .await?,
        )
    } else {
        None
    };

    // 10. Spawn signal handler for graceful shutdown (SIGINT + SIGTERM)
    let shutdown_cancel = cancel.clone();
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        shutdown_cancel.cancel();
    });

    // 11. Run all servers concurrently.
    let (error_tx, mut error_rx) = tokio::sync::mpsc::channel::<anyhow::Error>(1);

    let rtmp_handle = spawn_server(error_tx.clone(), rtmp_server.run());
    let grpc_handle = {
        let grpc_cancel = cancel.clone();
        spawn_server(
            error_tx.clone(),
            grpc_server.serve(grpc_cancel.child_token()),
        )
    };
    let http_flv_handle =
        http_flv_server.map(|server| spawn_server(error_tx.clone(), server.run()));
    let rtsp_handle = spawn_server(error_tx.clone(), rtsp_server.run());

    info!("All servers started (RTMP, RTSP, gRPC, HTTP-FLV)");

    // Wait for either a server error or graceful shutdown signal.
    let first_error: Option<anyhow::Error> = tokio::select! {
        Some(e) = error_rx.recv() => {
            error!(error = %e, "Server exited with error, shutting down...");
            Some(e)
        }
        _ = cancel.cancelled() => {
            info!("Graceful shutdown initiated");
            None
        }
    };

    // Cancel remaining servers and wait for drain.
    cancel.cancel();
    info!("Waiting for all servers to drain...");

    let drain_timeout = Duration::from_secs(10);
    let _ = tokio::time::timeout(drain_timeout, async {
        let _ = tokio::join!(
            async {
                let _ = rtmp_handle.await;
            },
            async {
                let _ = grpc_handle.await;
            },
            async {
                if let Some(h) = http_flv_handle {
                    let _ = h.await;
                }
            },
            async {
                let _ = rtsp_handle.await;
            },
        );
    })
    .await;

    if let Some(e) = first_error {
        return Err(anyhow::anyhow!("Server shut down with error: {}", e));
    }

    info!("All servers shut down gracefully");
    Ok(())
}

// ── helpers ──

fn spawn_server(
    error_tx: tokio::sync::mpsc::Sender<anyhow::Error>,
    fut: impl std::future::Future<Output = Result<(), anyhow::Error>> + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = fut.await {
            let _ = error_tx.send(e).await;
        }
    })
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut sigint = signal(SignalKind::interrupt()).ok();
        let mut sigterm = signal(SignalKind::terminate()).ok();

        let sigint_fut = async {
            if let Some(ref mut s) = sigint {
                s.recv().await;
            }
        };
        let sigterm_fut = async {
            if let Some(ref mut s) = sigterm {
                s.recv().await;
            }
        };

        tokio::select! {
            _ = sigint_fut => {
                info!("Received SIGINT, initiating graceful shutdown...");
            }
            _ = sigterm_fut => {
                info!("Received SIGTERM, initiating graceful shutdown...");
            }
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to register Ctrl+C handler");
        info!("Received Ctrl+C, initiating graceful shutdown...");
    }
}
