use std::{io, net::SocketAddr};

use anyhow::Result;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use super::RtmpConnection;

pub struct RtmpServer {
    listener: TcpListener,
    appname: String,
    cancel_token: CancellationToken,
}

impl RtmpServer {
    pub async fn create(
        addr: SocketAddr,
        appname: String,
        cancel_token: CancellationToken,
    ) -> Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self {
            listener,
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

                accpet_res = self.listener.accept() => {
                    self.handle_accept_result(accpet_res)?;
                }
            }
        }

        Ok(())
    }

    fn handle_accept_result(&self, res: Result<(TcpStream, SocketAddr), io::Error>) -> Result<()> {
        match res {
            Ok((socket, addr)) => {
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
            Err(e) => {
                error!("Error accepting RTMP connection: {:?}", e);
                Err(e.into())
            }
        }
    }

    async fn spawn_connection_handler(appname: String, connection: RtmpConnection) {
        let builder = if let Ok(builder) = connection.perform_handshake().await {
            builder.with_appname(appname)
        } else {
            warn!("RTMP handshake failed");
            return;
        };

        let mut session = if let Ok(sess) = builder.build() {
            sess
        } else {
            warn!("Failed to build RTMP session guard");
            return;
        };

        if let Err(e) = session.handle_loop().await {
            warn!(error = %e, "Error handling RTMP session");
        }
    }
}
