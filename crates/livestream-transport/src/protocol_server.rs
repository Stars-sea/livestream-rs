//! ProtocolServerCore — shared state and behavior for protocol ingest servers.
//!
//! Extracted from duplicated RTMP and RTSP server logic.
//! Each protocol server (RtmpServer, RtspServer) wraps a core and
//! delegates shared methods, providing protocol-specific connection
//! handling via closures.

use std::future::Future;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use dashmap::DashMap;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use livestream_telemetry::{
    metric_connection_accepted, metric_connection_closed, metric_connection_rejected,
};

use crate::config::ServerConfig;
use crate::controller::ControlMessage;
use crate::dispatcher::{EndReason, EventDispatcher};
use crate::flv::FlvEgressHub;
use crate::lifecycle::HandlerLifecycle;
use crate::registry::SessionRegistry;
use crate::registry::state::SessionEndpoint;
use livestream_codec::SegmentConfig;
use livestream_core::channel::MpscRx;
use livestream_core::config::TranscodeConfig;
use livestream_core::types::Protocol;
use livestream_pipeline::sink::minio::ObjectUploader;

pub(crate) struct ProtocolServerCore {
    pub listener: TcpListener,
    pub ctrl_channel: MpscRx<ControlMessage>,
    pub flv_egress_hub: Arc<FlvEgressHub>,
    pub pending_lifecycle: Arc<DashMap<String, HandlerLifecycle>>,
    pub precreate_ttl: Duration,
    pub minio: Arc<dyn ObjectUploader>,
    pub segment_cfg: SegmentConfig,
    pub transcode: TranscodeConfig,
    pub cancel_token: CancellationToken,
    pub registry: Arc<SessionRegistry>,
    pub dispatcher: Arc<EventDispatcher>,
    /// Tracks spawned connection handler + source tasks for graceful drain.
    tasks: JoinSet<()>,
    /// Optional connection limit semaphore (None = unlimited).
    connection_semaphore: Option<Arc<tokio::sync::Semaphore>>,
}

impl ProtocolServerCore {
    pub(crate) async fn from_config(cfg: ServerConfig) -> Result<Self> {
        let listener = TcpListener::bind(cfg.addr).await?;
        let connection_semaphore = crate::config::make_connection_semaphore(cfg.max_connections);
        Ok(Self {
            listener,
            ctrl_channel: cfg.ctrl_channel,
            flv_egress_hub: cfg.flv_egress_hub,
            pending_lifecycle: Arc::new(DashMap::new()),
            precreate_ttl: cfg.precreate_ttl,
            minio: cfg.minio,
            segment_cfg: cfg.segment_cfg,
            transcode: cfg.transcode,
            cancel_token: cfg.cancel_token,
            registry: cfg.registry,
            dispatcher: cfg.dispatcher,
            tasks: JoinSet::new(),
            connection_semaphore,
        })
    }

    /// Main event loop: cancel / control messages / accept connections.
    ///
    /// `protocol` determines the lifecycle protocol tag for precreated sessions.
    /// `on_accept` is called with each successfully accepted socket+addr.
    pub(crate) async fn run(
        &mut self,
        protocol: Protocol,
        on_accept: impl Fn(TcpStream, SocketAddr) -> Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> Result<()> {
        let protocol_name = match protocol {
            Protocol::Rtmp => "RTMP",
            Protocol::Rtsp => "RTSP",
            _ => "UNKNOWN",
        };

        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    debug!("{} server cancellation requested, shutting down", protocol_name);
                    break;
                }

                msg = self.ctrl_channel.recv() => {
                    let Some(msg) = msg else {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    };

                    if let Err(e) = self
                        .handle_control_message(msg, protocol)
                        .await
                    {
                        error!(
                            error = %e,
                            "Failed to handle {} control message",
                            protocol_name,
                        );
                    }
                }

                accept_res = self.listener.accept() => {
                    self.handle_accept_result(accept_res, protocol_name, &on_accept).await;
                }
            }
        }

        debug!(
            "{} server draining {} spawned tasks",
            protocol_name,
            self.tasks.len()
        );
        self.tasks.shutdown().await;

        Ok(())
    }

    async fn handle_accept_result(
        &mut self,
        accept_res: std::io::Result<(TcpStream, SocketAddr)>,
        protocol_name: &str,
        on_accept: &(
             impl Fn(TcpStream, SocketAddr) -> Pin<Box<dyn Future<Output = ()> + Send>> + ?Sized
         ),
    ) {
        fn is_retryable_accept_error(err: &std::io::Error) -> bool {
            matches!(
                err.kind(),
                ErrorKind::Interrupted
                    | ErrorKind::WouldBlock
                    | ErrorKind::TimedOut
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::ConnectionReset
            )
        }

        match accept_res {
            Ok((socket, addr)) => self.spawn_accepted(socket, addr, protocol_name, on_accept),
            Err(err) if is_retryable_accept_error(&err) => {
                warn!(
                    error = %err,
                    kind = ?err.kind(),
                    "Retryable {} accept error, server continues running",
                    protocol_name,
                );
                sleep(Duration::from_millis(20)).await;
            }
            Err(err) => {
                error!(
                    error = %err,
                    kind = ?err.kind(),
                    "Non-retryable {} accept error, server stays alive with backoff",
                    protocol_name,
                );
                sleep(Duration::from_millis(200)).await;
            }
        }
    }

    fn spawn_accepted(
        &mut self,
        socket: TcpStream,
        addr: SocketAddr,
        protocol_name: &str,
        on_accept: &(
             impl Fn(TcpStream, SocketAddr) -> Pin<Box<dyn Future<Output = ()> + Send>> + ?Sized
         ),
    ) {
        let permit = self.acquire_permit(addr, protocol_name);
        if permit.is_none() && self.connection_semaphore.is_some() {
            drop(socket);
            return;
        }
        // When no connection limit is configured, every connection is accepted.
        if self.connection_semaphore.is_none() {
            metric_connection_accepted!(protocol_name.to_string());
        }
        let fut = on_accept(socket, addr);
        let protocol = protocol_name.to_string();
        self.tasks.spawn(async move {
            let _permit = permit;
            fut.await;
            metric_connection_closed!(protocol.clone());
        });
    }

    fn acquire_permit(
        &self,
        addr: SocketAddr,
        protocol_name: &str,
    ) -> Option<tokio::sync::OwnedSemaphorePermit> {
        let sem = self.connection_semaphore.as_ref()?;
        match sem.clone().try_acquire_owned() {
            Ok(p) => {
                metric_connection_accepted!(protocol_name.to_string());
                Some(p)
            }
            Err(_) => {
                metric_connection_rejected!(protocol_name.to_string());
                warn!(
                    client_addr = %addr,
                    "{protocol_name} connection rejected: at capacity ({})",
                    sem.available_permits(),
                );
                None
            }
        }
    }

    async fn handle_control_message(
        &mut self,
        msg: ControlMessage,
        protocol: Protocol,
    ) -> Result<()> {
        match msg {
            ControlMessage::PrecreateStream {
                live_id,
                ack,
                passphrase,
            } => {
                // Control messages are handled serially by the run loop, so
                // this existence check is race-free: a concurrent precreate
                // for the same live_id can never slip past it before the
                // registration below. Resolving the ack here makes the
                // registration outcome authoritative (the caller gets
                // already_exists instead of observing a concurrent winner's
                // descriptor). Rejecting up front also avoids creating a
                // HandlerLifecycle that would tear down the existing session
                // on drop.
                if self.registry.get_session(&live_id).is_some() {
                    ack.send(Err(anyhow::anyhow!(
                        "Stream key {} is already in use",
                        live_id
                    )));
                    return Ok(());
                }

                // Pre-create the FLV broadcast channel so subscribers can join
                // before the publisher connects.
                self.flv_egress_hub.create_channel(&live_id);

                let session_token = self.cancel_token.child_token();

                let lifecycle = HandlerLifecycle::new(
                    live_id.clone(),
                    protocol,
                    self.registry.clone(),
                    self.dispatcher.clone(),
                );

                if let Err(e) = lifecycle
                    .pending(
                        SessionEndpoint::new(None, passphrase),
                        session_token.clone(),
                    )
                    .await
                {
                    warn!(
                        live_id = %live_id,
                        error = %e,
                        "Failed to precreate session, resolving ack with failure",
                    );
                    ack.send(Err(e));
                    return Ok(());
                }

                let descriptor = match self.registry.get_descriptor(&live_id).await {
                    Some(descriptor) => descriptor,
                    None => {
                        ack.send(Err(anyhow::anyhow!(
                            "precreate succeeded but no session descriptor found"
                        )));
                        return Ok(());
                    }
                };
                ack.send(Ok(descriptor));

                let protocol_name = match protocol {
                    Protocol::Rtmp => "RTMP",
                    Protocol::Rtsp => "RTSP",
                    _ => "UNKNOWN",
                };
                self.spawn_precreate_session_ttl(live_id, lifecycle, session_token, protocol_name);

                Ok(())
            }
            ControlMessage::StopStream { live_id } => {
                if let Some(token) = self.registry.get_cancel_token(&live_id) {
                    token.cancel();
                }

                // Drop the FLV channel (and its demand signal) so stopped
                // streams don't leak hub state or serve stale cached sequence
                // headers to a same-named stream later.
                self.flv_egress_hub.remove_channel(&live_id);

                Ok(())
            }
        }
    }

    fn spawn_precreate_session_ttl(
        &mut self,
        live_id: String,
        lifecycle: HandlerLifecycle,
        session_token: CancellationToken,
        protocol_name: &str,
    ) {
        let pending_lifecycle = self.pending_lifecycle.clone();
        let flv_egress_hub = self.flv_egress_hub.clone();
        pending_lifecycle.insert(live_id.clone(), lifecycle);

        let ttl = self.precreate_ttl;
        if ttl.is_zero() {
            debug!(
                "Precreate session TTL is set to 0, skipping TTL expiration for live_id {}",
                live_id
            );
            return;
        }

        let name = protocol_name.to_string();
        tokio::spawn(async move {
            tokio::select! {
                _ = session_token.cancelled() => { return; }
                _ = sleep(ttl) => {}
            }

            if !pending_lifecycle.contains_key(&live_id) {
                return;
            }

            warn!(
                live_id = %live_id,
                ttl_secs = ttl.as_secs(),
                "Expired pending {} precreated session by TTL",
                name,
            );

            let Some((_, lifecycle)) = pending_lifecycle.remove(&live_id) else {
                debug!(live_id = %live_id, "Pending lifecycle already removed for live_id, skipping TTL expiration");
                return;
            };
            lifecycle.disconnect_with_reason(EndReason::Timeout);
            // The precreated session is gone for good; drop the FLV channel
            // and its demand signal so hub state doesn't leak and a
            // same-named stream starts without stale cached seq headers.
            flv_egress_hub.remove_channel(&live_id);
        });
    }
}
