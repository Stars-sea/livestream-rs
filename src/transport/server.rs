use std::net::SocketAddr;
use std::str::FromStr;

use anyhow::Result;
use crossfire::MTx;
use crossfire::{AsyncRx, mpsc, spsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

use super::TransportController;
use super::contract::message::{ControlMessage, StreamEvent, StreamFlvTag};
use super::contract::state::{ConnectionStateTrait, SessionState};
use super::registry::global;
use super::rtmp::RtmpServer;
use super::srt::SrtServer;
use crate::config::{QueueConfig, RtmpConfig, SrtConfig};
use crate::dispatcher::{self, Protocal, SessionEvent};
use crate::infra::PortAllocator;
use crate::pipeline::PipeBus;

pub struct TransportServer {
    rtmp_config: RtmpConfig,
    srt_config: SrtConfig,
    queue_config: QueueConfig,
    rtmp_tag_rx: Option<AsyncRx<mpsc::Array<StreamFlvTag>>>,

    bus: PipeBus,

    cancel_token: CancellationToken,
}

impl TransportServer {
    pub fn new(
        rtmp_config: RtmpConfig,
        srt_config: SrtConfig,
        queue_config: QueueConfig,
        rtmp_tag_rx: AsyncRx<mpsc::Array<StreamFlvTag>>,
        bus: PipeBus,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            rtmp_config,
            srt_config,
            queue_config,
            rtmp_tag_rx: Some(rtmp_tag_rx),
            bus,
            cancel_token,
        }
    }

    async fn rtmp_server(
        &mut self,
        rx: AsyncRx<spsc::Array<ControlMessage>>,
        event_tx: MTx<mpsc::Array<StreamEvent>>,
    ) -> Result<RtmpServer> {
        let appname = self.rtmp_config.appname.clone();
        let cancel_token = self.cancel_token.child_token();
        let rtmp_tag_rx = self
            .rtmp_tag_rx
            .take()
            .ok_or_else(|| anyhow::anyhow!("RTMP forwarded tag receiver already taken"))?;

        let addr = SocketAddr::from_str(&format!("0.0.0.0:{}", self.rtmp_config.port))?;
        let server = RtmpServer::create(
            addr,
            appname,
            rx,
            event_tx,
            rtmp_tag_rx,
            self.queue_config.rtmppublish,
            self.bus.clone(),
            cancel_token,
        )
        .await?;
        Ok(server)
    }

    async fn srt_server(
        &self,
        rx: AsyncRx<spsc::Array<ControlMessage>>,
        event_tx: MTx<mpsc::Array<StreamEvent>>,
    ) -> Result<SrtServer> {
        let cancel_token = self.cancel_token.child_token();

        let port_allocator = match self.srt_config.srt_port_range() {
            Ok((start, end)) => PortAllocator::new(start, end),
            Err(e) => {
                error!("Failed to create port allocator: {}", e);
                anyhow::bail!("Failed to create port allocator: {}", e);
            }
        };

        let server = SrtServer::new(
            rx,
            event_tx,
            self.queue_config.srtpacket,
            self.bus.clone(),
            port_allocator,
            cancel_token,
        );
        Ok(server)
    }

    pub async fn spawn_task(mut self) -> Result<(TransportController, JoinHandle<Result<()>>)> {
        let (rtmp_msg_tx, rtmp_msg_rx) =
            spsc::bounded_blocking_async(self.queue_config.control);
        let (srt_msg_tx, srt_msg_rx) =
            spsc::bounded_blocking_async(self.queue_config.control);
        let (event_tx, event_rx) = mpsc::bounded_blocking_async(self.queue_config.event);

        let rtmp_server = self.rtmp_server(rtmp_msg_rx, event_tx.clone()).await?;
        let srt_server = self.srt_server(srt_msg_rx, event_tx.clone()).await?;

        let handle = tokio::spawn(async move {
            tokio::try_join!(
                rtmp_server.run(),
                srt_server.run(),
                event_listener(event_rx)
            )?;
            Ok(())
        });

        let controller = TransportController::new(rtmp_msg_tx, srt_msg_tx);
        Ok((controller, handle))
    }
}

async fn handle_stream_event(event: StreamEvent) -> Result<()> {
    let dispatcher = dispatcher::singleton().await;

    match event {
        StreamEvent::StateChange { live_id, new_state } => {
            debug!(live_id = %live_id, new_state = ?new_state, "Stream state changed");
            if let Err(e) = global::update_session_state(&live_id, new_state).await {
                error!(error = %e, live_id = %live_id, "Failed to update session state, cancelling stream");

                if let Some(ct) = global::get_cancel_token(&live_id).await {
                    ct.cancel();
                } else {
                    anyhow::bail!("No cancellation token found for live_id: {}", &live_id);
                }
            }

            if new_state.is_active() {
                dispatcher.send(SessionEvent::SessionStarted {
                    live_id: live_id.clone(),
                    protocal: match new_state {
                        SessionState::Rtmp(_) => Protocal::Rtmp,
                        SessionState::Srt(_) => Protocal::Srt,
                    },
                });
            }

            if new_state.is_stopped() {
                dispatcher.send(SessionEvent::SessionEnded {
                    live_id: live_id.clone(),
                    protocal: match new_state {
                        SessionState::Rtmp(_) => Protocal::Rtmp,
                        SessionState::Srt(_) => Protocal::Srt,
                    },
                });
            }
            Ok(())
        }
        StreamEvent::Init { live_id, streams } => {
            dispatcher.send(SessionEvent::SessionInit { live_id, streams });
            Ok(())
        }
    }
}

async fn event_listener(event_rx: AsyncRx<mpsc::Array<StreamEvent>>) -> Result<()> {
    loop {
        match event_rx.recv().await {
            Ok(event) => {
                if let Err(e) = handle_stream_event(event).await {
                    error!(error = %e, "Failed to handle stream event");
                }
            }
            Err(e) => {
                error!(error = %e, "Failed to receive stream event");
                break;
            }
        }
    }
    Ok(())
}
