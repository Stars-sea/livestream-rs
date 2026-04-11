use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use super::channel::LiveChannel;
use super::connection::RtmpConnection;
use crate::pipeline::PipeBus;
use crate::queue::MpscChannel;
use crate::transport::contract::message::{
    ControlMessage, StreamEvent, StreamFlvTag, send_stream_state_change,
};
use crate::transport::contract::state::{
    RtmpState, SessionDescriptor, SessionEndpoint, SessionProtocol, SessionState,
};
use crate::transport::registry::global;
use crate::transport::rtmp::handler::HandlerBuilder;

pub struct RtmpServer {
    listener: TcpListener,
    appname: String,

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
                        self.handle_control_message(msg).await?;
                    }
                }

                accept_res = self.listener.accept() => {
                    let (socket, addr) = accept_res?;
                    self.accept_client(socket, addr)?;
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

    async fn handle_control_message(&mut self, msg: ControlMessage) -> Result<()> {
        match msg {
            ControlMessage::PrecreateStream { live_id, .. } => {
                // Pre-allocate synchronization resources to circumvent lock contention
                // when the crucial stream headers arrive.
                self.bus.prepare_stream(&live_id);

                let session = SessionDescriptor {
                    id: live_id.clone(),
                    protocol: SessionProtocol::Rtmp,
                    endpoint: SessionEndpoint {
                        port: None,
                        passphrase: None,
                    },
                    state: SessionState::Rtmp(RtmpState::Pending),
                };
                global::register_session(session, self.cancel_token.child_token()).await?;

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

    fn accept_client(&self, socket: TcpStream, addr: SocketAddr) -> Result<()> {
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

        Ok(())
    }

    async fn handle_forwarded_tag(&mut self, packet: StreamFlvTag) {
        let StreamFlvTag { stream_id, tag } = packet;

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

fn emit_rtmp_disconnect_event(event_channel: &MpscChannel<StreamEvent>, stream_key: &str) {
    if let Err(e) = send_stream_state_change(
        event_channel,
        stream_key.to_string(),
        SessionState::Rtmp(RtmpState::Disconnected),
        "rtmp.server.emit_disconnect",
    ) {
        warn!(stream_key = %stream_key, error = %e, "Failed to emit RTMP disconnected event");
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
                emit_rtmp_disconnect_event(&state_event_channel, &stream_key);
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
                    emit_rtmp_disconnect_event(&state_event_channel, &stream_key);
                }
            }
        }
        Err(e) => {
            warn!(error = %e, "Failed to build RTMP session handler");

            if is_publish {
                emit_rtmp_disconnect_event(&state_event_channel, &stream_key);
            }
        }
    }
}
