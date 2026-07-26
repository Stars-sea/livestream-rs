//! TransportServer — aggregates protocol servers and shared infrastructure.
//!
//! Provides a single `serve()` entry point for RTMP + RTSP concurrency,
//! plus access to the `TransportController` for gRPC integration.

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

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
    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        rtmp_port: u16,
        rtmp_app_name: String,
        rtsp_port: u16,
        http_flv_enabled: bool,
        http_flv_port: u16,
        session_ttl_secs: u64,
        flv_egress_hub: Arc<FlvEgressHub>,
        factory: Arc<PipelineFactory>,
        cancel: CancellationToken,
    ) -> Result<Self> {
        let session_ttl = Duration::from_secs(session_ttl_secs);
        let registry = Arc::new(SessionRegistry::new());
        let dispatcher = Arc::new(EventDispatcher::new());

        let (rtmp_tx, rtmp_rx) = channel::mpsc("ctrl_rtmp", None, Default::default());
        let (rtsp_tx, rtsp_rx) = channel::mpsc("ctrl_rtsp", None, Default::default());

        let controller = Arc::new(TransportController::new(registry.clone(), rtmp_tx, rtsp_tx));

        let rtmp_addr = SocketAddr::from_str(&format!("0.0.0.0:{}", rtmp_port))?;
        let rtmp_server = RtmpServer::create(
            rtmp_addr,
            rtmp_app_name,
            session_ttl,
            rtmp_rx,
            flv_egress_hub.clone(),
            factory.minio().clone(),
            factory.segment_cfg().clone(),
            cancel.child_token(),
            registry.clone(),
            dispatcher.clone(),
        )
        .await?;

        let rtsp_addr = SocketAddr::from_str(&format!("0.0.0.0:{}", rtsp_port))?;
        let rtsp_server = RtspServer::create(
            rtsp_addr,
            rtsp_rx,
            flv_egress_hub.clone(),
            registry.clone(),
            dispatcher.clone(),
            session_ttl,
            factory.minio().clone(),
            factory.segment_cfg().clone(),
            cancel.child_token(),
        )
        .await?;

        let http_flv_server = if http_flv_enabled {
            Some(
                HttpFlvServer::create(
                    http_flv_port,
                    flv_egress_hub,
                    registry.clone(),
                    cancel.child_token(),
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
            cancel,
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
