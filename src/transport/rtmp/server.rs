use std::io::ErrorKind;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Duration, sleep};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use super::connection::RtmpConnection;
use crate::channel::MpscRx;
use crate::dispatcher::Protocol;
use crate::metric_ttl_expiration;
use crate::pipeline::PipeBus;
use crate::transport::controller::ControlMessage;
use crate::transport::flv::FlvEgressHub;
use crate::transport::lifecycle::HandlerLifecycle;
use crate::transport::registry;
use crate::transport::registry::state::SessionEndpoint;
use crate::transport::rtmp::handler::HandlerBuilder;

pub struct RtmpServer {
    listener: TcpListener,
    appname: String,
    precreate_ttl: Duration,
    ctrl_channel: MpscRx<ControlMessage>,
    flv_egress_hub: Arc<FlvEgressHub>,
    bus: PipeBus,
    pending_lifecycle: Arc<DashMap<String, HandlerLifecycle>>,
    cancel_token: CancellationToken,
}

impl RtmpServer {
    pub async fn create(
        addr: SocketAddr,
        appname: String,
        precreate_ttl: Duration,
        ctrl_channel: MpscRx<ControlMessage>,
        flv_egress_hub: Arc<FlvEgressHub>,
        bus: PipeBus,
        cancel_token: CancellationToken,
    ) -> Result<Self> {
        let listener = TcpListener::bind(addr).await?;

        Ok(Self {
            listener,
            appname,
            precreate_ttl,
            ctrl_channel,
            flv_egress_hub,
            bus,
            pending_lifecycle: Arc::new(DashMap::new()),
            cancel_token,
        })
    }

    pub async fn run(mut self) -> Result<()> {
        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    debug!("RTMP server cancellation requested, shutting down");
                    break;
                }

                msg = self.ctrl_channel.next() => {
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
                self.bus.prepare_stream(&live_id);

                let session_token = self.cancel_token.child_token();

                let lifecycle = HandlerLifecycle::new(live_id.clone(), Protocol::Rtmp);
                lifecycle
                    .pending(SessionEndpoint::default(), session_token.clone())
                    .await?;

                self.spawn_precreate_session_ttl(live_id, lifecycle, session_token);

                Ok(())
            }
            ControlMessage::StopStream { live_id } => {
                if let Some(token) = registry::INSTANCE.get_cancel_token(&live_id) {
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

            metric_ttl_expiration!("rtmp", "precreate_pending_timeout");

            let Some((_, lifecycle)) = pending_lifecycle.remove(&live_id) else {
                debug!(live_id = %live_id, "Pending lifecycle already removed for live_id, skipping TTL expiration");
                return;
            };
            lifecycle.disconnect();

            warn!(
                live_id = %live_id,
                ttl_secs = ttl.as_secs(),
                "Expired pending RTMP precreated session by TTL"
            );
        });
    }

    fn accept_client(&self, socket: TcpStream, addr: SocketAddr) {
        debug!(client_addr = %addr, "Accepted new RTMP connection");

        tokio::spawn(spawn_connection_handler(
            self.appname.clone(),
            socket,
            self.bus.clone(),
            self.pending_lifecycle.clone(),
            self.flv_egress_hub.clone(),
        ));
    }
}

async fn spawn_connection_handler(
    appname: String,
    socket: TcpStream,
    bus: PipeBus,
    pending_lifecycle: Arc<DashMap<String, HandlerLifecycle>>,
    flv_egress_hub: Arc<FlvEgressHub>,
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

    let builder = builder.with_appname(appname);
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

    let Some(cancel_token) = registry::INSTANCE.get_cancel_token(&stream_key) else {
        error!(stream_key = %stream_key, "No cancellation token found for stream key");
        return;
    };

    let _cancel_guard = match &builder {
        HandlerBuilder::Play { .. } => None,
        HandlerBuilder::Publish { .. } => Some(cancel_token.clone().drop_guard()),
    };

    let builder = if is_publish {
        let Some((_, lifecycle)) = pending_lifecycle.remove(&stream_key) else {
            warn!(stream_key = %stream_key, "No pending lifecycle found, exiting...");
            return;
        };
        builder.with_lifecycle(lifecycle)
    } else {
        let (tag_stream, cached_tags) = flv_egress_hub.subscribe(&stream_key).await;
        builder
            .with_tag_stream(tag_stream.with_live_id(stream_key.clone()))
            .with_cached_tags(cached_tags)
    };

    let builder = builder.with_cancel_token(cancel_token).with_pipe_bus(bus);

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
}
