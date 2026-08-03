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
    config::ServerConfig,
    controller::TransportController,
    dispatcher::EventDispatcher,
    flv::FlvEgressHub,
    grpc::{GrpcServer, GrpcServerConfig},
    http_flv::HttpFlvServer,
    registry::SessionRegistry,
    rtmp::RtmpServer,
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
        Some(minio_cfg) => match PersistenceClient::create(minio_cfg.clone()).await {
            Ok(client) => Arc::new(client) as Arc<dyn ObjectUploader>,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to create MinIO client, HLS upload disabled");
                factory::null_uploader()
            }
        },
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
    let rtmp_server = match RtmpServer::create(
        ServerConfig {
            addr: rtmp_addr,
            ctrl_channel: rtmp_rx,
            flv_egress_hub: flv_egress_hub.clone(),
            registry: registry.clone(),
            dispatcher: dispatcher.clone(),
            precreate_ttl: session_ttl,
            minio: minio.clone(),
            segment_cfg: segment_cfg.clone(),
            cancel_token: cancel.child_token(),
            max_connections: config.transport.rtmp.max_connections,
        },
        config.transport.rtmp.app_name.clone(),
    )
    .await
    {
        Ok(server) => Some(server),
        Err(e) => {
            tracing::warn!(error = %e, "RTMP server failed to start, RTMP ingest disabled");
            None
        }
    };

    // 6. Create RTSP control channel and server
    let (rtsp_tx, rtsp_rx) = channel::mpsc("ctrl_rtsp", None, config.queue.control);
    let rtsp_addr = SocketAddr::from_str(&format!("0.0.0.0:{}", config.transport.rtsp.port))?;
    let rtsp_ttl = Duration::from_secs(config.transport.rtsp.session_ttl_secs);

    let rtsp_server = match RtspServer::create(ServerConfig {
        addr: rtsp_addr,
        ctrl_channel: rtsp_rx,
        flv_egress_hub: flv_egress_hub.clone(),
        registry: registry.clone(),
        dispatcher: dispatcher.clone(),
        precreate_ttl: rtsp_ttl,
        minio: minio.clone(),
        segment_cfg: segment_cfg.clone(),
        cancel_token: cancel.child_token(),
        max_connections: config.transport.rtsp.max_connections,
    })
    .await
    {
        Ok(server) => Some(server),
        Err(e) => {
            tracing::warn!(error = %e, "RTSP server failed to start, RTSP ingest disabled");
            None
        }
    };

    let controller = Arc::new(TransportController::new(registry.clone(), rtmp_tx, rtsp_tx));

    // 8. Create gRPC server. Failure here stays fatal (propagated with `?`):
    // gRPC is the control plane that configures/observes the transports, so
    // the process cannot operate usefully without it.
    let grpc_server = GrpcServer::new(GrpcServerConfig {
        port: config.services.grpc.port,
        rtmp_port: rtmp_server.as_ref().map(|_| config.transport.rtmp.port),
        rtmp_app_name: config.transport.rtmp.app_name.clone(),
        rtsp_port: rtsp_server.as_ref().map(|_| config.transport.rtsp.port),
        http_flv_enabled: config.services.http_flv.enabled,
        http_flv_port: config.services.http_flv.port,
        control: controller,
        registry: registry.clone(),
        dispatcher: dispatcher.clone(),
    })?;

    // 9. Create HTTP-FLV server (optional). Like RTMP/RTSP, a bind failure
    // degrades to a warning and the server stays disabled rather than
    // aborting the process. (gRPC, by contrast, stays fatal: see step 8.)
    let http_flv_server = if config.services.http_flv.enabled {
        match HttpFlvServer::create(
            config.services.http_flv.port,
            config.services.http_flv.max_connections,
            flv_egress_hub.clone(),
            registry.clone(),
            cancel.child_token(),
        )
        .await
        {
            Ok(server) => Some(server),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "HTTP-FLV server failed to start, HTTP-FLV playback disabled"
                );
                None
            }
        }
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

    let rtmp_handle = rtmp_server.map(|server| spawn_server(error_tx.clone(), server.run()));
    let grpc_handle = {
        let grpc_cancel = cancel.clone();
        spawn_server(
            error_tx.clone(),
            grpc_server.serve(grpc_cancel.child_token()),
        )
    };
    let http_flv_handle =
        http_flv_server.map(|server| spawn_server(error_tx.clone(), server.run()));
    let rtsp_handle = rtsp_server.map(|server| spawn_server(error_tx.clone(), server.run()));

    info!("All servers started (RTMP, RTSP, gRPC, HTTP-FLV)");

    // Wait for either a server error or graceful shutdown signal.
    let first_error: Option<anyhow::Error> = tokio::select! {
        msg = error_rx.recv() => {
            match msg {
                Some(e) => {
                    error!(error = %e, "Server exited with error, shutting down...");
                    Some(e)
                }
                None => {
                    info!("All server tasks completed");
                    None
                }
            }
        }
        _ = cancel.cancelled() => {
            info!("Graceful shutdown initiated");
            None
        }
    };
    // Cancel remaining servers and wait for drain.
    cancel.cancel();
    info!("Waiting for all servers to drain...");

    let started_count = usize::from(rtmp_handle.is_some())
        + 1 // gRPC is always started
        + usize::from(http_flv_handle.is_some())
        + usize::from(rtsp_handle.is_some());

    let drain_timeout = Duration::from_secs(10);
    let (rtmp_drained, grpc_drained, http_flv_drained, rtsp_drained) =
        tokio::time::timeout(drain_timeout, async {
            tokio::join!(
                drain_handle(rtmp_handle),
                drain_handle(Some(grpc_handle)),
                drain_handle(http_flv_handle),
                drain_handle(rtsp_handle),
            )
        })
        .await
        .unwrap_or((false, false, false, false));

    let drained_count = usize::from(rtmp_drained)
        + usize::from(grpc_drained)
        + usize::from(http_flv_drained)
        + usize::from(rtsp_drained);
    let not_drained = started_count.saturating_sub(drained_count);

    if not_drained > 0 {
        tracing::warn!(
            not_drained = %not_drained,
            timeout_secs = drain_timeout.as_secs(),
            "Server(s) did not stop within the drain timeout; handles dropped"
        );
    }

    if let Some(e) = first_error {
        return Err(anyhow::anyhow!("Server shut down with error: {}", e));
    }

    if not_drained == 0 {
        info!("All servers shut down gracefully");
    }
    Ok(())
}

// ── helpers ──

fn spawn_server(
    error_tx: tokio::sync::mpsc::Sender<anyhow::Error>,
    fut: impl std::future::Future<Output = Result<(), anyhow::Error>> + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = fut.await {
            // The error channel has capacity 1 and the main loop stops polling
            // it after the first error, so a second failure would block on
            // `.send().await` forever. Report best-effort and log locally if
            // the channel is full or closed.
            if let Err(send_err) = error_tx.try_send(e) {
                error!(
                    error = %send_err.into_inner(),
                    "Failed to report server error (error channel full or closed)"
                );
            }
        }
    })
}

/// Awaits a server handle; returns `true` if the task finished (or there was
/// no task to await), `false` if it did not complete cleanly.
async fn drain_handle(handle: Option<tokio::task::JoinHandle<()>>) -> bool {
    match handle {
        Some(h) => h.await.is_ok(),
        None => true,
    }
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
