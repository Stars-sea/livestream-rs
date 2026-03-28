use std::{net::SocketAddr, sync::Arc};

use anyhow::Result;
use crossfire::{AsyncRx, MTx, mpsc, spsc};
use dashmap::DashMap;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::transport::message::{ControlMessage, StreamEvent};
use crate::transport::{ConnectionState, SessionDescriptor, SessionState, global};

use super::RtmpConnection;

pub struct RtmpServer {
    listener: TcpListener,
    appname: String,

    ctrl_rx: AsyncRx<spsc::List<ControlMessage>>,
    event_rx: AsyncRx<mpsc::List<StreamEvent>>,

    // For sending events back to the main server loop
    // Passing the sender to the connection handlers
    event_tx: MTx<mpsc::List<StreamEvent>>,

    cancel_token: CancellationToken,

    child_tokens: Arc<DashMap<String, CancellationToken>>,
}

impl RtmpServer {
    pub async fn create(
        addr: SocketAddr,
        appname: String,
        ctrl_rx: AsyncRx<spsc::List<ControlMessage>>,
        cancel_token: CancellationToken,
    ) -> Result<Self> {
        let listener = TcpListener::bind(addr).await?;

        let (event_tx, event_rx) = mpsc::unbounded_async();

        Ok(Self {
            listener,
            appname,
            ctrl_rx,
            event_rx,
            event_tx,
            cancel_token,
            child_tokens: Arc::new(DashMap::new()),
        })
    }

    pub async fn run(&self) -> Result<()> {
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

                event = self.event_rx.recv() => {
                    match event {
                        Ok(event) => self.handle_stream_event(event).await?,
                        Err(e) => warn!(error = %e, "Error receiving stream event"),
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

    async fn handle_control_message(&self, msg: ControlMessage) -> Result<()> {
        match msg {
            ControlMessage::PrecreateStream { live_id } => {
                let session = SessionDescriptor {
                    id: live_id,
                    state: SessionState::Rtmp(ConnectionState::Precreate),
                };
                global::register_session(Arc::new(RwLock::new(session))).await?;

                Ok(())
            }
            ControlMessage::StopStream { live_id } => {
                let entry = self.child_tokens.remove(&live_id);
                if let Some((_, token)) = entry {
                    token.cancel();
                }

                Ok(())
            }
        }
    }

    async fn handle_stream_event(&self, event: StreamEvent) -> Result<()> {
        // TODO
        match event {
            StreamEvent::StateChange { live_id, new_state } => {
                debug!(live_id = %live_id, new_state = ?new_state, "Stream state changed");
                Ok(())
            }
            StreamEvent::Exited { live_id } => {
                debug!(live_id = %live_id, "Stream exited");
                Ok(())
            }
        }
    }

    fn accept_client(&self, socket: TcpStream, addr: SocketAddr) -> Result<()> {
        debug!(client_addr = %addr, "Accepted new RTMP connection");

        let cancel_token = self.cancel_token.child_token();

        // Handle the connection in a separate task
        tokio::spawn(spawn_connection_handler(
            self.appname.clone(),
            socket,
            self.event_tx.clone(),
            cancel_token,
            self.child_tokens.clone(),
        ));

        Ok(())
    }
}

async fn spawn_connection_handler(
    appname: String,
    socket: TcpStream,
    event_tx: MTx<mpsc::List<StreamEvent>>,
    cancel_token: CancellationToken,
    child_tokens: Arc<DashMap<String, CancellationToken>>,
) {
    let connection = RtmpConnection::new(socket, cancel_token.clone());

    let builder = match connection.perform_handshake().await {
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

    // TODO: call with_flv_tag_rx
    let builder = match session.connect().await {
        Ok(builder) => builder,
        Err(e) => {
            warn!(error = %e, "Failed to connect RTMP session");
            return;
        }
    };

    // TODO: Consider implementing a unified CancellationToken management strategy for both RTMP and Srt protocols.

    // child_tokens shouldn't be modified when Handler is Play.
    // For Publish, the token is stored in child_tokens to allow cancellation from the main server loop.
    // For Play, the session cancellation is managed through the SessionGuard and the event channel.
    if builder.is_publish() {
        child_tokens.insert(builder.stream_key().to_string(), cancel_token);
    }

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
