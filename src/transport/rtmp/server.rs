use std::{net::SocketAddr, sync::Arc};

use anyhow::{Result, anyhow};
use crossfire::{AsyncRx, MTx, mpsc, spsc};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::transport::message::{ControlMessage, StreamEvent};
use crate::transport::{RtmpState, SessionDescriptor, SessionState, global};

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
                    state: SessionState::Rtmp(RtmpState::Pending),
                };
                global::register_session(Arc::new(RwLock::new(session))).await?;

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

    async fn handle_stream_event(&self, event: StreamEvent) -> Result<()> {
        match event {
            StreamEvent::StateChange { live_id, new_state } => {
                debug!(live_id = %live_id, new_state = ?new_state, "Stream state changed");
                if let Err(e) = global::update_session_state(&live_id, new_state).await {
                    error!(error = %e, live_id = %live_id, "Failed to update session state, cancelling stream");
                    let cancel_token = global::get_cancel_token(&live_id).await.ok_or(anyhow!(
                        "No cancellation token found for live_id: {}",
                        live_id
                    ))?;
                    cancel_token.cancel();
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
            self.cancel_token.child_token(),
        ));

        Ok(())
    }
}

async fn spawn_connection_handler(
    appname: String,
    socket: TcpStream,
    event_tx: MTx<mpsc::List<StreamEvent>>,
    cancel_token: CancellationToken,
) {
    let _cancel_guard = cancel_token.clone().drop_guard();

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

    let stream_key = builder.stream_key();
    let cancel_token = if builder.is_publish() {
        // For Publish sessions, we create a cancellation token and register it in the registry.
        match global::register_cancel_token(stream_key, cancel_token.clone()).await {
            Ok(()) => cancel_token,
            Err(e) => {
                error!(error = %e, "Failed to register cancellation token");
                return;
            }
        }
    } else {
        // For Play sessions, we don't register a cancellation token.
        // Instead, we get the token from the registry.
        drop(_cancel_guard);
        match global::get_cancel_token(stream_key).await {
            Some(token) => token,
            None => {
                error!(stream_key = %stream_key, "No cancellation token found for stream key");
                return;
            }
        }
    };

    // TODO: call with_flv_tag_rx
    let builder = builder.with_cancel_token(cancel_token);

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
