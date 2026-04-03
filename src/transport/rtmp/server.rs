use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use crossfire::{AsyncRx, MAsyncTx, MTx, mpsc, spsc};
use dashmap::DashMap;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

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
    play_tag_senders: Arc<DashMap<String, broadcast::Sender<FlvTag>>>,
    rtmp_tag_rx: AsyncRx<mpsc::Array<StreamFlvTag>>,

    bus: PipeBus,

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
            play_tag_senders: Arc::new(DashMap::new()),
            rtmp_tag_rx,
            bus,
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
                        Ok(tag) => self.handle_forwarded_tag(tag),
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
                let session = SessionDescriptor {
                    id: live_id,
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
        let play_tag_senders = self.play_tag_senders.clone();

        // Handle the connection in a separate task
        tokio::spawn(spawn_connection_handler(
            self.appname.clone(),
            socket,
            self.event_tx.clone(),
            self.publish_tag_tx.clone().into_async(),
            move |stream_key| {
                let tx = play_tag_senders.entry(stream_key).or_insert_with(|| {
                    let (sender, _) = broadcast::channel(1024);
                    sender
                });
                tx.subscribe()
            },
        ));

        Ok(())
    }

    fn handle_forwarded_tag(&self, packet: StreamFlvTag) {
        let StreamFlvTag { stream_id, tag } = packet;

        if let Some(tx) = self.play_tag_senders.get(&stream_id) {
            if tx.send(tag).is_err() {
                debug!(stream_id = %stream_id, "No active RTMP play subscribers");
            }
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

async fn spawn_connection_handler(
    appname: String,
    socket: TcpStream,
    event_tx: MTx<mpsc::Array<StreamEvent>>,
    tag_tx: MAsyncTx<mpsc::Array<WrappedFlvTag>>,
    tag_rx: impl Fn(String) -> broadcast::Receiver<FlvTag>,
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

    let stream_key = builder.stream_key();
    let cancel_token = match global::get_cancel_token(stream_key).await {
        Some(token) => token,
        None => {
            error!(stream_key = %stream_key, "No cancellation token found for stream key");
            return;
        }
    };
    let _cancel_guard = match &builder {
        HandlerBuilder::Play { .. } => None,
        HandlerBuilder::Publish { .. } => Some(cancel_token.clone().drop_guard()),
    };

    let stream_key = builder.stream_key().to_string();
    let builder = builder
        .with_cancel_token(cancel_token)
        .with_tag_tx(tag_tx)
        .with_tag_rx(tag_rx(stream_key));

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
