use std::net::SocketAddr;
use std::str::FromStr;

use anyhow::Result;
use crossfire::{AsyncRx, spsc::List, spsc::unbounded_async};
use tokio_util::sync::CancellationToken;
use tracing::error;

use super::TransportController;
use super::contract::message::ControlMessage;
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

    async fn rtmp_server(&self, rx: AsyncRx<List<ControlMessage>>) -> Result<RtmpServer> {
        let appname = self.rtmp_config.appname.clone();
        let cancel_token = self.cancel_token.child_token();

        let addr = SocketAddr::from_str(&format!("0.0.0.0:{}", self.rtmp_config.port))?;
        let server = RtmpServer::create(addr, appname, rx, cancel_token).await?;
        Ok(server)
    }

    async fn srt_server(&self, rx: AsyncRx<List<ControlMessage>>) -> Result<SrtServer> {
        let host = self.srt_config.host.clone();
        let cancel_token = self.cancel_token.child_token();

        let port_allocator = match self.srt_config.srt_port_range() {
            Ok((start, end)) => PortAllocator::new(start, end),
            Err(e) => {
                error!("Failed to create port allocator: {}", e);
                anyhow::bail!("Failed to create port allocator: {}", e);
            }
        };

        let server = SrtServer::new(rx, host, port_allocator, cancel_token);
        Ok(server)
    }

    pub async fn spawn_task(self) -> Result<TransportController> {
        let (rtmp_msg_tx, rtmp_msg_rx) = unbounded_async();
        let (srt_msg_tx, srt_msg_rx) = unbounded_async();

        let rtmp_server = self.rtmp_server(rtmp_msg_rx).await?;
        let srt_server = self.srt_server(srt_msg_rx).await?;

        let handle = tokio::spawn(async move {
            tokio::try_join!(rtmp_server.run(), srt_server.run())?;
            Ok(())
        });

        Ok(TransportController::new(
            rtmp_msg_tx,
            srt_msg_tx,
            handle,
            self.cancel_token,
        ))
    }
}
