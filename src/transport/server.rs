use std::net::SocketAddr;
use std::str::FromStr;

use anyhow::Result;
use crossfire::AsyncRx;
use crossfire::spsc::List;
use tokio_util::sync::CancellationToken;

use super::message::ControlMessage;
use super::rtmp::RtmpServer;
use super::srt::SrtServer;
use crate::config::{RtmpConfig, SrtConfig};
use crate::infra::PortAllocator;

pub struct TransportServer {
    rtmp_config: RtmpConfig,
    srt_config: SrtConfig,

    cancel_token: CancellationToken,
}

impl TransportServer {
    pub fn new(
        rtmp_config: RtmpConfig,
        srt_config: SrtConfig,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            rtmp_config,
            srt_config,
            cancel_token,
        }
    }

    async fn run_rtmp_server(&self, rx: AsyncRx<List<ControlMessage>>) -> Result<()> {
        let appname = self.rtmp_config.appname.clone();
        let cancel_token = self.cancel_token.child_token();

        let addr = SocketAddr::from_str(&format!("0.0.0.0:{}", self.rtmp_config.port))?;
        let server = RtmpServer::create(addr, appname, rx, cancel_token).await?;
        server.run().await
    }

    async fn run_srt_server(&self, rx: AsyncRx<List<ControlMessage>>) -> Result<()> {
        let host = self.srt_config.host.clone();
        let cancel_token = self.cancel_token.child_token();

        let port_allocator = match self.srt_config.srt_port_range() {
            Ok((start, end)) => PortAllocator::new(start, end),
            Err(e) => {
                eprintln!("Failed to create port allocator: {}", e);
                anyhow::bail!("Failed to create port allocator: {}", e);
            }
        };

        let server = SrtServer::new(rx, host, port_allocator, cancel_token);
        server.run().await?;
        Ok(())
    }

    pub async fn run(self) -> Result<()> {
        let (rtmp_msg_tx, rtmp_msg_rx) = crossfire::spsc::unbounded_async();
        let (srt_msg_tx, srt_msg_rx) = crossfire::spsc::unbounded_async();

        tokio::try_join!(
            self.run_rtmp_server(rtmp_msg_rx),
            self.run_srt_server(srt_msg_rx)
        )?;
        Ok(())
    }
}
