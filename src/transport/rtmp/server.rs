use std::{net::SocketAddr, sync::Arc};

use anyhow::Result;
use crossfire::{AsyncRx, spsc::List};
use dashmap::DashMap;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::transport::message::ControlMessage;
use crate::transport::{ConnectionState, SessionDescriptor, SessionState, global_registry};

use super::RtmpConnection;

pub struct RtmpServer {
    listener: TcpListener,
    rx: AsyncRx<List<ControlMessage>>,
    appname: String,
    cancel_token: CancellationToken,

    child_tokens: Arc<DashMap<String, CancellationToken>>,
}

impl RtmpServer {
    pub async fn create(
        addr: SocketAddr,
        rx: AsyncRx<List<ControlMessage>>,
        appname: String,
        cancel_token: CancellationToken,
    ) -> Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self {
            listener,
            rx,
            appname,
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

                msg = self.rx.recv() => {
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

    async fn handle_control_message(&self, msg: ControlMessage) -> Result<()> {
        match msg {
            ControlMessage::PrecreateStream { live_id } => {
                let session = SessionDescriptor {
                    id: live_id,
                    state: SessionState::Rtmp(ConnectionState::Precreate),
                };
                global_registry()
                    .await
                    .register_session(Arc::new(RwLock::new(session)))
                    .await?;

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

    fn accept_client(&self, socket: TcpStream, addr: SocketAddr) -> Result<()> {
        debug!(client_addr = %addr, "Accepted new RTMP connection");

        let cancel_token = self.cancel_token.child_token();

        // Handle the connection in a separate task
        tokio::spawn(spawn_connection_handler(
            self.appname.clone(),
            socket,
            cancel_token,
            self.child_tokens.clone(),
        ));

        Ok(())
    }
}

async fn spawn_connection_handler(
    appname: String,
    socket: TcpStream,
    cancel_token: CancellationToken,
    child_tokens: Arc<DashMap<String, CancellationToken>>,
) {
    let connection = RtmpConnection::new(socket, cancel_token.clone());

    let builder = match connection.perform_handshake().await {
        Ok(builder) => builder.with_appname(appname),
        Err(e) => {
            warn!(error = %e, "RTMP handshake failed");
            return;
        }
    };

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
    child_tokens.insert(builder.stream_key().to_string(), cancel_token);

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
