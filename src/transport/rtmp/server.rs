use std::net::SocketAddr;

use anyhow::Result;
use crossfire::{AsyncRx, spsc::List};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::transport::message::ControlMessage;

use super::RtmpConnection;

pub struct RtmpServer {
    listener: TcpListener,
    rx: AsyncRx<List<ControlMessage>>,
    appname: String,
    cancel_token: CancellationToken,
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
                    // TODO: Handle control messages from the RTMP connection
                }

                accept_res = self.listener.accept() => {
                    let (socket, addr) = accept_res?;
                    self.accept_client(socket, addr)?;
                }
            }
        }

        Ok(())
    }

    fn accept_client(&self, socket: TcpStream, addr: SocketAddr) -> Result<()> {
        debug!(client_addr = %addr, "Accepted new RTMP connection");

        let cancel_token = self.cancel_token.child_token();
        let connection = RtmpConnection::new(socket, cancel_token);

        // Handle the connection in a separate task
        tokio::spawn(Self::spawn_connection_handler(
            self.appname.clone(),
            connection,
        ));

        Ok(())
    }

    async fn spawn_connection_handler(appname: String, connection: RtmpConnection) {
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
        let handler = match session.connect().await {
            Ok(builder) => builder.build(),
            Err(e) => Err(e.into()),
        };

        match handler {
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
}
