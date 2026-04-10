use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use crossfire::{AsyncRx, MAsyncTx, MTx, mpsc, spsc};
use dashmap::DashMap;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use super::channel::LiveChannel;
use super::connection::RtmpConnection;
use crate::infra::media::packet::FlvTag;
use crate::pipeline::{PipeBus, UnifiedPacketContext};
use crate::transport::contract::message::{ControlMessage, StreamEvent, StreamFlvTag};
use crate::transport::contract::state::{
    RtmpState, SessionDescriptor, SessionEndpoint, SessionProtocol, SessionState,
};
use crate::transport::registry::global;
use crate::transport::rtmp::handler::HandlerBuilder;
use crate::transport::rtmp::tag::WrappedFlvTag;

pub struct RtmpServer {
    listener: TcpListener,
    appname: String,

    ctrl_rx: AsyncRx<spsc::Array<ControlMessage>>,
    event_tx: MTx<mpsc::Array<StreamEvent>>,
    publish_tag_rx: AsyncRx<mpsc::Array<WrappedFlvTag>>,
    publish_tag_tx: MTx<mpsc::Array<WrappedFlvTag>>,
    rtmp_tag_rx: AsyncRx<mpsc::Array<StreamFlvTag>>,

    bus: PipeBus,
    active_channels: Arc<DashMap<String, Arc<LiveChannel>>>,

    cancel_token: CancellationToken,
}

impl RtmpServer {
    pub async fn create(
        addr: SocketAddr,
        appname: String,
        ctrl_rx: AsyncRx<spsc::Array<ControlMessage>>,
        event_tx: MTx<mpsc::Array<StreamEvent>>,
        rtmp_tag_rx: AsyncRx<mpsc::Array<StreamFlvTag>>,
        publish_queue_capacity: usize,
        bus: PipeBus,
        cancel_token: CancellationToken,
    ) -> Result<Self> {
        let listener = TcpListener::bind(addr).await?;

        let (tag_tx, tag_rx) = mpsc::bounded_blocking_async(publish_queue_capacity);

        Ok(Self {
            listener,
            appname,
            ctrl_rx,
            event_tx,
            publish_tag_rx: tag_rx,
            publish_tag_tx: tag_tx,
            rtmp_tag_rx,
            bus,
            active_channels: Arc::new(DashMap::new()),
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

                msg = self.ctrl_rx.recv() => {
                    match msg {
                        Ok(msg) => self.handle_control_message(msg).await?,
                        Err(e) => warn!(error = %e, "Error receiving control message"),
                    }
                }

                accept_res = self.listener.accept() => {
                    let (socket, addr) = accept_res?;
                    self.accept_client(socket, addr)?;
                }

                tag = self.publish_tag_rx.recv() => {
                    match tag {
                        Ok(tag) => {
                            if let Err(e) = self.handle_packet_received(tag).await {
                                debug!("Error handling received packet: {:?}", e);
                            }
                        }
                        Err(e) => debug!("Error receiving packet: {:?}", e),
                    }
                }

                tag = self.rtmp_tag_rx.recv() => {
                    match tag {
                        Ok(tag) => self.handle_forwarded_tag(tag).await,
                        Err(e) => debug!("Error receiving forwarded RTMP tag: {:?}", e),
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
            self.event_tx.clone(),
            self.publish_tag_tx.clone().into_async(),
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

    async fn handle_packet_received(&mut self, packet: WrappedFlvTag) -> Result<()> {
        let WrappedFlvTag {
            stream_key,
            tag,
            cancel_token,
        } = packet;

        if let FlvTag::ScriptData(meta) = &tag {
            self.event_tx.send(StreamEvent::Init {
                live_id: stream_key.clone(),
                streams: Arc::new(meta.clone()),
            })?;
        }

        // TODO: Consider using a more efficient way to send packets to the pipeline
        // TODO: Consider how to handle ScriptData properly
        let context = UnifiedPacketContext::new(stream_key, tag.into(), cancel_token);
        self.bus.send_packet(context).await?;
        Ok(())
    }
}

fn emit_rtmp_disconnect_event(event_tx: &MTx<mpsc::Array<StreamEvent>>, stream_key: &str) {
    if let Err(e) = event_tx.send(StreamEvent::StateChange {
        live_id: stream_key.to_string(),
        new_state: SessionState::Rtmp(RtmpState::Disconnected),
    }) {
        warn!(stream_key = %stream_key, error = %e, "Failed to emit RTMP disconnected event");
    }
}

async fn spawn_connection_handler(
    appname: String,
    socket: TcpStream,
    event_tx: MTx<mpsc::Array<StreamEvent>>,
    tag_tx: MAsyncTx<mpsc::Array<WrappedFlvTag>>,
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

    let state_event_tx = event_tx.clone();
    let builder = builder.with_appname(appname).with_event_tx(event_tx);
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
                emit_rtmp_disconnect_event(&state_event_tx, &stream_key);
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
    let (tag_receiver, cached_tags) = channel.subscribe().await;

    let handler_cancel_token = cancel_token.clone();
    let builder = builder
        .with_cancel_token(cancel_token)
        .with_tag_tx(tag_tx)
        .with_tag_rx(tag_receiver)
        .with_cached_tags(cached_tags);

    match builder.build() {
        Ok(mut handler) => {
            if let Err(e) = handler.handle().await {
                warn!(error = %e, "Error handling RTMP session");

                if is_publish && !handler_cancel_token.is_cancelled() {
                    emit_rtmp_disconnect_event(&state_event_tx, &stream_key);
                }
            }
        }
        Err(e) => {
            warn!(error = %e, "Failed to build RTMP session handler");

            if is_publish {
                emit_rtmp_disconnect_event(&state_event_tx, &stream_key);
            }
        }
    }
}
