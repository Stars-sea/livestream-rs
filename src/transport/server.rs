use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Result;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::error;

use super::TransportController;
use super::rtmp::RtmpServer;
use super::srt::SrtServer;
use crate::channel::{self, MpscRx};
use crate::config::{QueueConfig, RtmpConfig, SrtConfig};
use crate::infra::PortAllocator;
use crate::infra::media::packet::FlvTag;
use crate::pipeline::PipeBus;
use crate::transport::abstraction::IngestPacket;
use crate::transport::controller::ControlMessage;

pub struct TransportServer {
    rtmp_config: RtmpConfig,
    srt_config: SrtConfig,
    queue_config: QueueConfig,
    rtmp_tag_channel: Option<MpscRx<Box<dyn IngestPacket<FlvTag> + Send>>>,

    bus: PipeBus,

    cancel_token: CancellationToken,
}

impl TransportServer {
    pub fn new(
        rtmp_config: RtmpConfig,
        srt_config: SrtConfig,
        queue_config: QueueConfig,
        rtmp_tag_channel: MpscRx<Box<dyn IngestPacket<FlvTag> + Send>>,
        bus: PipeBus,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            rtmp_config,
            srt_config,
            queue_config,
            rtmp_tag_channel: Some(rtmp_tag_channel),
            bus,
            cancel_token,
        }
    }

    async fn rtmp_server(&mut self, control_channel: MpscRx<ControlMessage>) -> Result<RtmpServer> {
        let appname = self.rtmp_config.appname.clone();
        let precreate_ttl = Duration::from_secs(self.rtmp_config.session_ttl_secs);
        let cancel_token = self.cancel_token.child_token();
        let rtmp_tag_channel = self
            .rtmp_tag_channel
            .take()
            .ok_or_else(|| anyhow::anyhow!("RTMP forwarded tag receiver already taken"))?;

        let addr = SocketAddr::from_str(&format!("0.0.0.0:{}", self.rtmp_config.port))?;
        let server = RtmpServer::create(
            addr,
            appname,
            precreate_ttl,
            control_channel,
            rtmp_tag_channel,
            self.bus.clone(),
            cancel_token,
        )
        .await?;
        Ok(server)
    }

    async fn srt_server(&self, control_channel: MpscRx<ControlMessage>) -> Result<SrtServer> {
        let cancel_token = self.cancel_token.child_token();

        let port_allocator = match self.srt_config.srt_port_range() {
            Ok((start, end)) => PortAllocator::new(start, end),
            Err(e) => {
                error!("Failed to create port allocator: {}", e);
                anyhow::bail!("Failed to create port allocator: {}", e);
            }
        };

        let server = SrtServer::new(
            control_channel,
            self.bus.clone(),
            port_allocator,
            cancel_token,
        );
        Ok(server)
    }

    pub async fn spawn_task(mut self) -> Result<(TransportController, JoinHandle<Result<()>>)> {
        let (rtmp_tx, rtmp_rx) = channel::mpsc("control_rtmp", None, self.queue_config.control);
        let (srt_tx, srt_rx) = channel::mpsc("control_srt", None, self.queue_config.control);

        let rtmp_server = self.rtmp_server(rtmp_rx).await?;
        let srt_server = self.srt_server(srt_rx).await?;

        let handle = tokio::spawn(async move {
            tokio::try_join!(rtmp_server.run(), srt_server.run())?;
            Ok(())
        });

        let controller = TransportController::new(rtmp_tx, srt_tx);
        Ok((controller, handle))
    }
}
