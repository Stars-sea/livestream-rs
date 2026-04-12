use std::io::ErrorKind;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Duration, sleep};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use super::channel::LiveChannel;
use super::connection::RtmpConnection;
use crate::pipeline::PipeBus;
use crate::queue::MpscChannel;
use crate::telemetry::metrics;
use crate::transport::contract::message::{
    ControlMessage, StreamEvent, StreamFlvTag, send_stream_state_change,
};
use crate::transport::contract::state::{
    ConnectionStateTrait, RtmpState, SessionDescriptor, SessionEndpoint, SessionProtocol,
    SessionState,
};
use crate::transport::registry::global;
use crate::transport::rtmp::handler::HandlerBuilder;

pub struct RtmpServer {
    listener: TcpListener,
    appname: String,
    precreate_ttl: Duration,

    ctrl_channel: MpscChannel<ControlMessage>,
    event_channel: MpscChannel<StreamEvent>,
    rtmp_forward_channel: MpscChannel<StreamFlvTag>,

    bus: PipeBus,
    active_channels: Arc<DashMap<String, Arc<LiveChannel>>>,

    cancel_token: CancellationToken,
}

impl RtmpServer {
    pub async fn create(
        addr: SocketAddr,
        appname: String,
        precreate_ttl: Duration,
        ctrl_channel: MpscChannel<ControlMessage>,
        event_channel: MpscChannel<StreamEvent>,
        rtmp_forward_channel: MpscChannel<StreamFlvTag>,
        bus: PipeBus,
        cancel_token: CancellationToken,
    ) -> Result<Self> {
        let listener = TcpListener::bind(addr).await?;

        Ok(Self {
            listener,
            appname,
            precreate_ttl,
            ctrl_channel,
            event_channel,
            rtmp_forward_channel,
            bus,
            active_channels: Arc::new(DashMap::new()),
            cancel_token,
        })
    }

    pub async fn run(mut self) -> Result<()> {
        let mut ctrl_stream = self
            .ctrl_channel
            .subscribe("transport.rtmp.server.control_rx")
            .map_err(|e| anyhow::anyhow!("Failed to subscribe RTMP control channel: {}", e))?;
        let mut rtmp_forward_stream = self
            .rtmp_forward_channel
            .subscribe("transport.rtmp.server.forward_rx")
            .map_err(|e| anyhow::anyhow!("Failed to subscribe RTMP forward channel: {}", e))?;

        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    debug!("RTMP server cancellation requested, shutting down");
                    break;
                }

                msg = ctrl_stream.next() => {
                    if let Some(msg) = msg {
                        if let Err(e) = self.handle_control_message(msg).await {
                            error!(error = %e, "Failed to handle RTMP control message");
                        }
                    }
                }

                accept_res = self.listener.accept() => {
                    self.handle_accept_result(accept_res).await;
                }

                tag = rtmp_forward_stream.next() => {
                    if let Some(tag) = tag {
                        self.handle_forwarded_tag(tag).await;
                    }
                }
            }
        }

        Ok(())
    }

    async fn handle_accept_result(&self, accept_res: std::io::Result<(TcpStream, SocketAddr)>) {
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
                // Pre-allocate synchronization resources to circumvent lock contention
                // when the crucial stream headers arrive.
                self.bus.prepare_stream(&live_id);

                let session_token = self.cancel_token.child_token();

                let session = SessionDescriptor {
                    id: live_id.clone(),
                    protocol: SessionProtocol::Rtmp,
                    endpoint: SessionEndpoint {
                        port: None,
                        passphrase: None,
                    },
                    state: SessionState::Rtmp(RtmpState::Pending),
                };
                global::register_session(session, session_token.clone()).await?;
                self.spawn_precreate_session_ttl(live_id, session_token);

                Ok(())
            }
            ControlMessage::StopStream { live_id } => {
                let token = global::get_cancel_token(&live_id).await;
                if let Some(token) = token {
                    token.cancel();
                }

                Ok(())
            }
        }
    }

    fn spawn_precreate_session_ttl(&self, live_id: String, session_token: CancellationToken) {
        let ttl = self.precreate_ttl;

        tokio::spawn(async move {
            tokio::select! {
                _ = session_token.cancelled() => {
                    return;
                }
                _ = sleep(ttl) => {}
            }

            let state = global::get_session_state(&live_id).await;
            if matches!(state, Some(SessionState::Rtmp(RtmpState::Pending))) {
                session_token.cancel();
                metrics::get_metrics().record_ttl_expiration("rtmp", "precreate_pending_timeout");

                warn!(
                    live_id = %live_id,
                    ttl_secs = ttl.as_secs(),
                    "Expired pending RTMP precreated session by TTL"
                );
            }
        });
    }

    fn accept_client(&self, socket: TcpStream, addr: SocketAddr) {
        debug!(client_addr = %addr, "Accepted new RTMP connection");

        // Handle the connection in a separate task
        tokio::spawn(spawn_connection_handler(
            self.appname.clone(),
            socket,
            self.event_channel
                .clone()
                .with_source("transport.rtmp.spawn_connection.event_tx"),
            self.bus.clone(),
            self.active_channels.clone(),
        ));
    }

    async fn handle_forwarded_tag(&mut self, packet: StreamFlvTag) {
        let StreamFlvTag { stream_id, tag } = packet;

        let state = global::get_session_state(&stream_id).await;
        let Some(state) = state else {
            self.active_channels.remove(&stream_id);
            debug!(stream_id = %stream_id, "Dropping forwarded RTMP tag for unknown session");
            return;
        };

        if !state.is_active() {
            self.active_channels.remove(&stream_id);
            debug!(stream_id = %stream_id, state = ?state, "Dropping forwarded RTMP tag for inactive session");
            return;
        }

        let channel = self
            .active_channels
            .entry(stream_id.clone())
            .or_insert_with(|| Arc::new(LiveChannel::new()))
            .clone();

        if channel.broadcast_tag(tag).await.is_err() {
            debug!(stream_id = %stream_id, "No active RTMP play subscribers");
        }
    }
}

async fn emit_rtmp_disconnect_event(event_channel: &MpscChannel<StreamEvent>, stream_key: &str) {
    if let Err(e) = send_stream_state_change(
        event_channel,
        stream_key.to_string(),
        SessionState::Rtmp(RtmpState::Disconnected),
        "rtmp.server.emit_disconnect",
    ) {
        warn!(stream_key = %stream_key, error = %e, "Failed to emit RTMP disconnected event");

        if let Err(update_err) = global::update_session_state(
            stream_key,
            SessionState::Rtmp(RtmpState::Disconnected),
        )
        .await
        {
            warn!(stream_key = %stream_key, error = %update_err, "Failed to fallback-update RTMP disconnected state");
        }
    }
}

async fn spawn_connection_handler(
    appname: String,
    socket: TcpStream,
    event_channel: MpscChannel<StreamEvent>,
    bus: PipeBus,
    active_channels: Arc<DashMap<String, Arc<LiveChannel>>>,
) {
    let cancel_token = CancellationToken::new();
    let _cancel_guard = cancel_token.drop_guard_ref();

    let connection = RtmpConnection::new(socket);

    // TODO: If the handshake fails, we should still send a StreamEvent::StateChange to update the session state to Failed
    let builder = match connection.perform_handshake(&cancel_token).await {
        Ok(builder) => builder,
        Err(e) => {
            warn!(error = %e, "RTMP handshake failed");
            return;
        }
    };

    let state_event_channel = event_channel
        .clone()
        .with_source("transport.rtmp.connection.state_event_tx");
    let builder = builder
        .with_appname(appname)
        .with_event_channel(event_channel);
    let session = match builder.build() {
        Ok(session) => session,
        Err(e) => {
            warn!(error = %e, "Failed to build RTMP session guard");
            return;
        }
    };

    let builder = match session.connect(&cancel_token).await {
        Ok(builder) => builder,
        Err(e) => {
            warn!(error = %e, "Failed to connect RTMP session");
            return;
        }
    };

    drop(_cancel_guard);

    let stream_key = builder.stream_key().to_string();
    let is_publish = matches!(&builder, HandlerBuilder::Publish { .. });

    let cancel_token = match global::get_cancel_token(&stream_key).await {
        Some(token) => token,
        None => {
            error!(stream_key = %stream_key, "No cancellation token found for stream key");

            if is_publish {
                emit_rtmp_disconnect_event(&state_event_channel, &stream_key).await;
            }
            return;
        }
    };

    let _cancel_guard = match &builder {
        HandlerBuilder::Play { .. } => None,
        HandlerBuilder::Publish { .. } => Some(cancel_token.clone().drop_guard()),
    };

    let channel = active_channels
        .entry(stream_key.clone())
        .or_insert_with(|| Arc::new(LiveChannel::new()))
        .clone();
    let (tag_stream, cached_tags) = channel.subscribe("transport.rtmp.play.live_channel").await;
    let tag_stream = tag_stream.with_live_id(stream_key.clone());

    let handler_cancel_token = cancel_token.clone();
    let builder = builder
        .with_cancel_token(cancel_token)
        .with_pipe_bus(bus)
        .with_tag_stream(tag_stream)
        .with_cached_tags(cached_tags);

    match builder.build() {
        Ok(mut handler) => {
            if let Err(e) = handler.handle().await {
                warn!(error = %e, "Error handling RTMP session");

                if is_publish && !handler_cancel_token.is_cancelled() {
                    emit_rtmp_disconnect_event(&state_event_channel, &stream_key).await;
                }
            }
        }
        Err(e) => {
            warn!(error = %e, "Failed to build RTMP session handler");

            if is_publish {
                emit_rtmp_disconnect_event(&state_event_channel, &stream_key).await;
            }
        }
    }

    if is_publish {
        active_channels.remove(&stream_key);
        debug!(stream_key = %stream_key, "Removed RTMP live channel cache after publish session ended");
    }
}
