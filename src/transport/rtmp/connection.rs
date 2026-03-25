use anyhow::Result;
use bytes::{Bytes, BytesMut};
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

pub struct RtmpConnection {
    socket: TcpStream,

    appname: String,

    cancel_token: CancellationToken,
}

impl RtmpConnection {
    pub fn new(socket: TcpStream, appname: String, cancel_token: CancellationToken) -> Self {
        Self {
            socket,
            appname,
            cancel_token,
        }
    }

    pub async fn perform_handshake(mut self) -> Result<Self> {
        let mut buffer = BytesMut::with_capacity(1536);
        let mut handshake = Handshake::new(PeerType::Server);

        loop {
            let length = self.read(&mut buffer).await?;
            if length == 0 {
                anyhow::bail!("Connection closed");
            }

            buffer.truncate(length);
            let (completed, resp) = match handshake.process_bytes(&buffer) {
                Ok(HandshakeProcessResult::InProgress { response_bytes }) => {
                    (false, response_bytes)
                }
                Ok(HandshakeProcessResult::Completed { response_bytes, .. }) => {
                    (true, response_bytes)
                }
                Err(e) => anyhow::bail!("Handshake error: {:?}", e),
            };

            if !resp.is_empty() {
                self.write(&resp).await?;
            }

            if completed {
                return Ok(self);
            }
        }
    }

    // TODO:
    // This function must be called after the handshake is completed, otherwise it will not work correctly
    // We can implement a guard to ensure that the handshake is completed before calling this function
    pub async fn handle(&mut self) -> Result<()> {
        let mut buffer = BytesMut::with_capacity(4096);
        loop {
            let length = self.read(&mut buffer).await?;
            if length == 0 {
                anyhow::bail!("Connection closed")
            }

            // Process the buffer as needed (e.g., parse RTMP messages)
            buffer.clear();
        }
    }

    async fn read(&mut self, buf: &mut BytesMut) -> Result<usize> {
        tokio::select! {
            _ = self.cancel_token.cancelled() => {
                anyhow::bail!("Connection read cancelled");
            }
            res = self.socket.read(buf) => {
                res.map_err(|e| e.into())
            }
        }
    }

    async fn write(&mut self, buf: &[u8]) -> Result<()> {
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
