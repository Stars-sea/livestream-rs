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
}

impl RtmpConnection {
    pub fn new(socket: TcpStream) -> Self {
        Self { socket }
    }

    pub async fn perform_handshake(
        mut self,
        ct: &CancellationToken,
    ) -> Result<SessionGuardBuilder> {
        let mut buffer = BytesMut::with_capacity(1536);
        let mut handshake = Handshake::new(PeerType::Server);

        let mut handshake_completed = false;

        while !handshake_completed {
            let length = self.read(&mut buffer, ct).await?;
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
                self.write(&resp, ct).await?;
            }
        }

        let config = ServerSessionConfig::new();
        let (session, results) = ServerSession::new(config)?;

        self.handle(results, ct).await?;
        Ok(SessionGuardBuilder::new(self).with_session(session))
    }

    pub(super) async fn handle(
        &mut self,
        results: Vec<ServerSessionResult>,
        ct: &CancellationToken,
    ) -> Result<()> {
        for result in results {
            match result {
                ServerSessionResult::OutboundResponse(packet) => {
                    self.write(&packet.bytes, ct).await?;
                }
                _ => anyhow::bail!("Unhandled session result"),
            }
        }

        Ok(())
    }

    pub(super) async fn read(
        &mut self,
        buf: &mut BytesMut,
        ct: &CancellationToken,
    ) -> Result<usize> {
        buf.clear();
        tokio::select! {
            _ = ct.cancelled() => {
                anyhow::bail!("Connection read cancelled");
            }
            res = self.socket.read_buf(buf) => {
                res.map_err(|e| e.into())
            }
        }
    }

    pub(super) async fn write(&mut self, buf: &[u8], ct: &CancellationToken) -> Result<()> {
        tokio::select! {
            _ = ct.cancelled() => {
                anyhow::bail!("Connection write cancelled");
            }
            res = self.socket.write_all(buf) => {
                res.map_err(|e| e.into())
            }
        }
    }
}
