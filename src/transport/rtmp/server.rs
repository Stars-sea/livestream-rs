use std::net::SocketAddr;

use anyhow::Result;
use crossfire::{AsyncRx, MAsyncTx, MTx, mpmc, mpsc, spsc};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use super::connection::RtmpConnection;
use crate::infra::media::packet::FlvTag;
use crate::pipeline::PipeBus;
use crate::transport::contract::message::{ControlMessage, StreamEvent};
use crate::transport::contract::state::{RtmpState, SessionDescriptor, SessionState};
use crate::transport::registry::global;
use crate::transport::rtmp::handler::HandlerBuilder;
use crate::transport::rtmp::tag::WrappedFlvTag;

pub struct RtmpServer {
    listener: TcpListener,
    appname: String,

    ctrl_rx: AsyncRx<spsc::List<ControlMessage>>,
    event_tx: MTx<mpsc::List<StreamEvent>>,

    bus: PipeBus,

    cancel_token: CancellationToken,
}

impl RtmpServer {
    pub async fn create(
        addr: SocketAddr,
        appname: String,
        ctrl_rx: AsyncRx<spsc::List<ControlMessage>>,
        event_tx: MTx<mpsc::List<StreamEvent>>,
        bus: PipeBus,
        cancel_token: CancellationToken,
    ) -> Result<Self> {
        let listener = TcpListener::bind(addr).await?;

        Ok(Self {
            listener,
            appname,
            ctrl_rx,
            event_tx,
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
            }
        }

        Ok(())
    }

    async fn handle_control_message(&mut self, msg: ControlMessage) -> Result<()> {
        match msg {
            ControlMessage::PrecreateStream { live_id } => {
                let session = SessionDescriptor {
                    id: live_id,
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
            todo!(),
            |_| todo!(),
        ));

        Ok(())
    }
}

async fn spawn_connection_handler(
    appname: String,
    socket: TcpStream,
    event_tx: MTx<mpsc::List<StreamEvent>>,
    tag_tx: MAsyncTx<mpmc::List<WrappedFlvTag>>,
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
