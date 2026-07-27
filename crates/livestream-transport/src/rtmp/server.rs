use std::io::ErrorKind;
use std::net::SocketAddr;
use std::sync::Arc;

use crate::config::ServerConfig;
use anyhow::Result;
use dashmap::DashMap;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Duration, sleep};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use super::connection::RtmpConnection;
use crate::controller::ControlMessage;
use crate::dispatcher::EndReason;
use crate::dispatcher::EventDispatcher;
use crate::flv::FlvEgressHub;
use crate::lifecycle::HandlerLifecycle;
use crate::registry::SessionRegistry;
use crate::registry::state::SessionEndpoint;
use crate::rtmp::handler::HandlerBuilder;
use crate::source::rtmp::RtmpSource;
use livestream_codec::EncodedPacket;
use livestream_codec::SegmentConfig;
use livestream_core::channel::MpscRx;
use livestream_core::pad::PadSender;
use livestream_core::traits::{Node, Source};
use livestream_core::types::Protocol;
use livestream_pipeline::factory;
use livestream_pipeline::sink::minio::ObjectUploader;
pub struct RtmpServer {
    listener: TcpListener,
    appname: String,
    precreate_ttl: Duration,
    ctrl_channel: MpscRx<ControlMessage>,
    flv_egress_hub: Arc<FlvEgressHub>,
    pending_lifecycle: Arc<DashMap<String, HandlerLifecycle>>,
    minio: Arc<dyn livestream_pipeline::sink::minio::ObjectUploader>,
    segment_cfg: livestream_codec::SegmentConfig,
    cancel_token: CancellationToken,
    registry: Arc<SessionRegistry>,
    dispatcher: Arc<EventDispatcher>,
}
impl RtmpServer {
    pub async fn create(cfg: ServerConfig, appname: String) -> Result<Self> {
        let listener = TcpListener::bind(cfg.addr).await?;

        Ok(Self {
            listener,
            appname,
            precreate_ttl: cfg.precreate_ttl,
            ctrl_channel: cfg.ctrl_channel,
            flv_egress_hub: cfg.flv_egress_hub,
            pending_lifecycle: Arc::new(DashMap::new()),
            minio: cfg.minio,
            segment_cfg: cfg.segment_cfg,
            cancel_token: cfg.cancel_token,
            registry: cfg.registry,
            dispatcher: cfg.dispatcher,
        })
    }

    pub async fn run(mut self) -> Result<()> {
        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    debug!("RTMP server cancellation requested, shutting down");
                    break;
                }

                msg = self.ctrl_channel.recv() => {
                    if let Some(msg) = msg
                        && let Err(e) = self.handle_control_message(msg).await {
                            error!(error = %e, "Failed to handle RTMP control message");
                        }
                }

                accept_res = self.listener.accept() => {
                    self.handle_accept_result(accept_res).await;
                }
            }
        }

        Ok(())
    }

    async fn handle_accept_result(&mut self, accept_res: std::io::Result<(TcpStream, SocketAddr)>) {
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
            Ok((socket, addr)) => self.accept_client(socket, addr),
            Err(err) if is_retryable_accept_error(&err) => {
                warn!(error = %err, kind = ?err.kind(), "Retryable RTMP accept error, server continues running");
                sleep(Duration::from_millis(20)).await;
            }
            Err(err) => {
                error!(error = %err, kind = ?err.kind(), "Non-retryable RTMP accept error, server stays alive with backoff");
                sleep(Duration::from_millis(200)).await;
            }
        }
    }

    async fn handle_control_message(&mut self, msg: ControlMessage) -> Result<()> {
        match msg {
            ControlMessage::PrecreateStream { live_id, .. } => {
                // Pre-create the FLV broadcast channel so subscribers can join
                // before the publisher connects.
                self.flv_egress_hub.create_channel(&live_id);

                let session_token = self.cancel_token.child_token();

                let lifecycle = HandlerLifecycle::new(
                    live_id.clone(),
                    Protocol::Rtmp,
                    self.registry.clone(),
                    self.dispatcher.clone(),
                );
                lifecycle
                    .pending(SessionEndpoint::default(), session_token.clone())
                    .await?;

                self.spawn_precreate_session_ttl(live_id, lifecycle, session_token);

                Ok(())
            }
            ControlMessage::StopStream { live_id } => {
                if let Some(token) = self.registry.get_cancel_token(&live_id) {
                    token.cancel();
                }

                Ok(())
            }
        }
    }

    fn spawn_precreate_session_ttl(
        &mut self,
        live_id: String,
        lifecycle: HandlerLifecycle,
        session_token: CancellationToken,
    ) {
        let pending_lifecycle = self.pending_lifecycle.clone();
        pending_lifecycle.insert(live_id.clone(), lifecycle);

        let ttl = self.precreate_ttl;
        if ttl.is_zero() {
            debug!(
                "Precreate session TTL is set to 0, skipping TTL expiration for live_id {}",
                live_id
            );
            return;
        }

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
                "Expired pending RTMP precreated session by TTL"
            );

            let Some((_, lifecycle)) = pending_lifecycle.remove(&live_id) else {
                debug!(live_id = %live_id, "Pending lifecycle already removed for live_id, skipping TTL expiration");
                return;
            };
            lifecycle.disconnect_with_reason(EndReason::Timeout);
        });
    }

    fn accept_client(&self, socket: TcpStream, addr: SocketAddr) {
        debug!(client_addr = %addr, "Accepted new RTMP connection");

        tokio::spawn(spawn_connection_handler(
            self.appname.clone(),
            socket,
            self.pending_lifecycle.clone(),
            self.flv_egress_hub.clone(),
            self.minio.clone(),
            self.segment_cfg.clone(),
            self.registry.clone(),
        ));
    }
}

async fn spawn_connection_handler(
    appname: String,
    socket: TcpStream,
    pending_lifecycle: Arc<DashMap<String, HandlerLifecycle>>,
    flv_egress_hub: Arc<FlvEgressHub>,
    minio: Arc<dyn ObjectUploader>,
    segment_cfg: SegmentConfig,
    registry: Arc<SessionRegistry>,
) {
    let cancel_token = CancellationToken::new();
    let _cancel_guard = cancel_token.drop_guard_ref();

    let connection = RtmpConnection::new(socket);

    let builder = match connection.perform_handshake(&cancel_token).await {
        Ok(builder) => builder,
        Err(e) => {
            warn!(error = %e, "RTMP handshake failed");
            return;
        }
    };

    let builder = builder
        .with_appname(appname)
        .with_registry(registry.clone());
    let session = match builder.build() {
        Ok(session) => session,
        Err(e) => {
            warn!(error = %e, "Failed to build RTMP session guard");
            return;
        }
    };

    let builder = match session.connect(&pending_lifecycle, &cancel_token).await {
        Ok(builder) => builder,
        Err(e) => {
            warn!(error = %e, "Failed to connect RTMP session");
            return;
        }
    };

    drop(_cancel_guard);

    let stream_key = builder.stream_key().to_string();
    let is_publish = matches!(&builder, HandlerBuilder::Publish { .. });

    let Some(cancel_token) = registry.get_cancel_token(&stream_key) else {
        error!(stream_key = %stream_key, "No cancellation token found for stream key");
        return;
    };

    // For publish, the registry token is cancelled when we exit so the
    // session lifecycle cleanup fires. We track this explicitly rather
    // than via DropGuard so we can control cancel-vs-drain ordering.
    let publish_token = is_publish.then(|| cancel_token.clone());

    let builder = if is_publish {
        let Some((_, lifecycle)) = pending_lifecycle.remove(&stream_key) else {
            warn!(stream_key = %stream_key, "No pending lifecycle found, exiting...");
            return;
        };
        // Build the pipeline via PipelineFactory.
        let pipeline_cancel = cancel_token.child_token();
        let (src_tx, src_rx) = PadSender::<EncodedPacket>::new_channel(512);

        let (rtmp_source, frame_tx) =
            RtmpSource::new(&stream_key, vec![], src_tx, pipeline_cancel.clone());
        let source = Arc::new(rtmp_source);

        // Spawn source
        spawn_source_task(source);

        // Factory: always succeeds for FLV path (OTelProbe → SeqCacheProbe → FlvMux → FlvSink).
        // HLS is skipped gracefully when codec params are empty (RTMP metadata arrives later).
        let _pipeline = factory::build_pipeline(
            &stream_key,
            src_rx,
            &[], // codec_params — empty for RTMP; HLS branch skipped with info log
            flv_egress_hub.clone(),
            minio,
            &segment_cfg,
            pipeline_cancel,
        )
        .expect("Pipeline factory should never fail for FLV-only path");

        builder.with_lifecycle(lifecycle).with_source_tx(frame_tx)
    } else {
        let Some((tag_stream, cached_tags)) = flv_egress_hub.subscribe(&stream_key) else {
            warn!(stream_key = %stream_key, "No FLV channel found for play request");
            return;
        };
        builder
            .with_tag_stream(tag_stream)
            .with_cached_tags(cached_tags)
    };

    let builder = builder.with_cancel_token(cancel_token.clone());

    match builder.build() {
        Ok(mut handler) => {
            if let Err(e) = handler.handle().await {
                warn!(error = %e, "Error handling RTMP session");
            }
        }
        Err(e) => {
            warn!(error = %e, "Failed to build RTMP session handler");
        }
    }

    // Cancel the pipeline before removing the FLV channel so buffered
    // FlvTags drain to subscribers before the channel is destroyed.
    cancel_token.cancel();

    // Allow buffered pipeline data to drain before removing the channel.
    tokio::time::sleep(Duration::from_millis(100)).await;

    flv_egress_hub.remove_channel(&stream_key);

    // Ensure registry token is cancelled for publish sessions.
    drop(publish_token);
}

fn spawn_source_task(source: Arc<RtmpSource>) {
    tokio::spawn(async move {
        if let Err(e) = source.start().await {
            tracing::error!(stream = %source.name(), error = %e, "RtmpSource failed");
        }
    });
}
