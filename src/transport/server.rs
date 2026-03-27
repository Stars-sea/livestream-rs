use std::net::SocketAddr;
use std::str::FromStr;

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::config::{RtmpConfig, SrtConfig};
use crate::transport::rtmp::RtmpServer;

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

    async fn run_rtmp_server(&self) -> Result<()> {
        let appname = self.rtmp_config.appname.clone();
        let cancel_token = self.cancel_token.child_token();

        let addr = SocketAddr::from_str(&format!("0.0.0.0:{}", self.rtmp_config.port))?;
        let server = RtmpServer::create(addr, appname, cancel_token).await?;
        server.run().await
    }

    pub async fn run(self) -> Result<()> {
        //TODO: Start both RTMP and SRT servers
        tokio::try_join!(self.run_rtmp_server())?;
        Ok(())
    }
}
