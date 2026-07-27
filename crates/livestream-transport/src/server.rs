//! TransportServer — aggregates protocol servers and shared infrastructure.
//!
//! Provides a single `serve()` entry point for RTMP + RTSP concurrency,
//! plus access to the `TransportController` for gRPC integration.

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use crate::config::ServerConfig;
use crate::controller::TransportController;
use crate::dispatcher::EventDispatcher;
use crate::flv::FlvEgressHub;
use crate::http_flv::HttpFlvServer;
use crate::registry::SessionRegistry;
use crate::rtmp::RtmpServer;
use crate::rtsp::server::RtspServer;
use anyhow::Result;
use livestream_core::channel;
use livestream_pipeline::factory::PipelineFactory;
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Configuration for constructing a [`TransportServer`].
pub struct TransportServerConfig {
    pub rtmp_port: u16,
    pub rtmp_app_name: String,
    pub rtsp_port: u16,
    pub http_flv_enabled: bool,
    pub http_flv_port: u16,
    pub session_ttl_secs: u64,
    pub flv_egress_hub: Arc<FlvEgressHub>,
    pub factory: Arc<PipelineFactory>,
    pub cancel: CancellationToken,
}

pub struct TransportServer {
    rtmp: RtmpServer,
    rtsp: RtspServer,
    http_flv: Option<HttpFlvServer>,
    registry: Arc<SessionRegistry>,
    dispatcher: Arc<EventDispatcher>,
    controller: Arc<TransportController>,
    cancel: CancellationToken,
}

impl TransportServer {
    pub async fn create(cfg: TransportServerConfig) -> Result<Self> {
        let session_ttl = Duration::from_secs(cfg.session_ttl_secs);
        let registry = Arc::new(SessionRegistry::new());
        let dispatcher = Arc::new(EventDispatcher::new());

        let (rtmp_tx, rtmp_rx) = channel::mpsc("ctrl_rtmp", None, Default::default());
        let (rtsp_tx, rtsp_rx) = channel::mpsc("ctrl_rtsp", None, Default::default());

        let controller = Arc::new(TransportController::new(registry.clone(), rtmp_tx, rtsp_tx));

        let rtmp_addr = SocketAddr::from_str(&format!("0.0.0.0:{}", cfg.rtmp_port))?;
        let rtmp_server = RtmpServer::create(
            ServerConfig {
                addr: rtmp_addr,
                ctrl_channel: rtmp_rx,
                flv_egress_hub: cfg.flv_egress_hub.clone(),
                registry: registry.clone(),
                dispatcher: dispatcher.clone(),
                precreate_ttl: session_ttl,
                minio: cfg.factory.minio().clone(),
                segment_cfg: cfg.factory.segment_cfg().clone(),
                cancel_token: cfg.cancel.child_token(),
            },
            cfg.rtmp_app_name,
        )
        .await?;

        let rtsp_addr = SocketAddr::from_str(&format!("0.0.0.0:{}", cfg.rtsp_port))?;
        let rtsp_server = RtspServer::create(ServerConfig {
            addr: rtsp_addr,
            ctrl_channel: rtsp_rx,
            flv_egress_hub: cfg.flv_egress_hub.clone(),
            registry: registry.clone(),
            dispatcher: dispatcher.clone(),
            precreate_ttl: session_ttl,
            minio: cfg.factory.minio().clone(),
            segment_cfg: cfg.factory.segment_cfg().clone(),
            cancel_token: cfg.cancel.child_token(),
        })
        .await?;

        let http_flv_server = if cfg.http_flv_enabled {
            Some(
                HttpFlvServer::create(
                    cfg.http_flv_port,
                    cfg.flv_egress_hub,
                    registry.clone(),
                    cfg.cancel.child_token(),
                )
                .await?,
            )
        } else {
            None
        };

        Ok(Self {
            rtmp: rtmp_server,
            rtsp: rtsp_server,
            http_flv: http_flv_server,
            registry,
            dispatcher,
            controller,
            cancel: cfg.cancel,
        })
    }

    pub fn controller(&self) -> Arc<TransportController> {
        self.controller.clone()
    }

    pub fn registry(&self) -> Arc<SessionRegistry> {
        self.registry.clone()
    }

    pub fn dispatcher(&self) -> Arc<EventDispatcher> {
        self.dispatcher.clone()
    }

    pub async fn serve(self) -> Result<()> {
        let (error_tx, mut error_rx) = tokio::sync::mpsc::channel::<anyhow::Error>(1);

        let rtmp_handle = spawn_server(error_tx.clone(), self.rtmp.run());
        let rtsp_handle = spawn_server(error_tx.clone(), self.rtsp.run());
        let http_flv_handle = self
            .http_flv
            .map(|s| spawn_server(error_tx.clone(), s.run()));

        info!("TransportServer started (RTMP, RTSP, HTTP-FLV)");

        let first_error = tokio::select! {
            Some(e) = error_rx.recv() => Some(e),
            _ = self.cancel.cancelled() => None,
        };

        self.cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(10), async {
            let _ = rtmp_handle.await;
            let _ = rtsp_handle.await;
            if let Some(h) = http_flv_handle {
                let _ = h.await;
            }
        })
        .await;

        if let Some(e) = first_error {
            Err(anyhow::anyhow!(
                "TransportServer shut down with error: {}",
                e
            ))
        } else {
            info!("TransportServer shut down gracefully");
            Ok(())
        }
    }
}

fn spawn_server(
    error_tx: tokio::sync::mpsc::Sender<anyhow::Error>,
    fut: impl std::future::Future<Output = Result<()>> + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = fut.await {
            let _ = error_tx.send(e).await;
        }
    })
}
