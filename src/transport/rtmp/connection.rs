use anyhow::Result;
use bytes::BytesMut;
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{ServerSession, ServerSessionConfig, ServerSessionResult};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use super::session::SessionGuardBuilder;

pub struct RtmpConnection {
    socket: TcpStream,

    cancel_token: CancellationToken,
}

impl RtmpConnection {
    pub fn new(socket: TcpStream, cancel_token: CancellationToken) -> Self {
        Self {
            socket,
            cancel_token,
        }
    }

    pub async fn perform_handshake(mut self) -> Result<SessionGuardBuilder> {
        let mut buffer = BytesMut::with_capacity(1536);
        let mut handshake = Handshake::new(PeerType::Server);

        let mut handshake_completed = false;

        while !handshake_completed {
            let length = self.read(&mut buffer).await?;
            if length == 0 {
                anyhow::bail!("Connection closed");
            }

            buffer.truncate(length);
            let resp = match handshake.process_bytes(&buffer) {
                Ok(HandshakeProcessResult::InProgress { response_bytes }) => response_bytes,
                Ok(HandshakeProcessResult::Completed { response_bytes, .. }) => {
                    handshake_completed = true;
                    response_bytes
                }
                Err(e) => anyhow::bail!("Handshake error: {:?}", e),
            };

            if !resp.is_empty() {
                self.write(&resp).await?;
            }
        }

        let config = ServerSessionConfig::new();
        let (session, results) = ServerSession::new(config)?;

        self.handle(results).await?;
        Ok(SessionGuardBuilder::new(self).with_session(session))
    }

    pub(super) async fn handle(&mut self, results: Vec<ServerSessionResult>) -> Result<()> {
        for result in results {
            match result {
                ServerSessionResult::OutboundResponse(packet) => {
                    self.write(&packet.bytes).await?;
                }
                _ => anyhow::bail!("Unhandled session result"),
            }
        }

        Ok(())
    }

    pub(super) async fn read(&mut self, buf: &mut BytesMut) -> Result<usize> {
        tokio::select! {
            _ = self.cancel_token.cancelled() => {
                anyhow::bail!("Connection read cancelled");
            }
            res = self.socket.read(buf) => {
                res.map_err(|e| e.into())
            }
        }
    }

    pub(super) async fn write(&mut self, buf: &[u8]) -> Result<()> {
        tokio::select! {
            _ = self.cancel_token.cancelled() => {
                anyhow::bail!("Connection write cancelled");
            }
            res = self.socket.write_all(buf) => {
                res.map_err(|e| e.into())
            }
        }
    }
}
