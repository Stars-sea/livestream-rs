use std::net::SocketAddr;
use std::str::FromStr;

use anyhow::Result;
use crossfire::AsyncRx;
use crossfire::spsc::List;
use tokio_util::sync::CancellationToken;

use crate::config::{RtmpConfig, SrtConfig};
use crate::transport::message::ControlMessage;
use crate::transport::registry;
use crate::transport::rtmp::RtmpServer;
use crate::transport::srt::SrtServer;

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
        let server = RtmpServer::create(addr, rx, appname, cancel_token).await?;
        server.run().await
    }

    async fn run_srt_server(&self, rx: AsyncRx<List<ControlMessage>>) -> Result<()> {
        let cancel_token = self.cancel_token.child_token();

        let server = SrtServer::new(rx, cancel_token);
        server.run().await?;
        Ok(())
    }

    pub async fn run(self) -> Result<()> {
        registry::init_registry();

        let (rtmp_msg_tx, rtmp_msg_rx) = crossfire::spsc::unbounded_async();
        let (srt_msg_tx, srt_msg_rx) = crossfire::spsc::unbounded_async();

        tokio::try_join!(
            self.run_rtmp_server(rtmp_msg_rx),
            self.run_srt_server(srt_msg_rx)
        )?;
        Ok(())
    }
}
