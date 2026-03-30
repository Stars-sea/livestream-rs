use std::net::SocketAddr;
use std::str::FromStr;

use anyhow::Result;
use crossfire::MTx;
use crossfire::{AsyncRx, mpsc, spsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

use super::TransportController;
use super::contract::message::ControlMessage;
use super::contract::state::ConnectionStateTrait;
use super::rtmp::RtmpServer;
use super::srt::SrtServer;
use crate::config::{RtmpConfig, SrtConfig};
use crate::dispatcher::{self, Protocal, SessionEvent};
use crate::infra::PortAllocator;
use crate::transport::contract::message::StreamEvent;
use crate::transport::contract::state::SessionState;
use crate::transport::registry::global;

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

    async fn rtmp_server(
        &self,
        rx: AsyncRx<spsc::List<ControlMessage>>,
        event_tx: MTx<mpsc::List<StreamEvent>>,
    ) -> Result<RtmpServer> {
        let appname = self.rtmp_config.appname.clone();
        let cancel_token = self.cancel_token.child_token();

        let addr = SocketAddr::from_str(&format!("0.0.0.0:{}", self.rtmp_config.port))?;
        let server = RtmpServer::create(addr, appname, rx, event_tx, cancel_token).await?;
        Ok(server)
    }

    async fn srt_server(
        &self,
        rx: AsyncRx<spsc::List<ControlMessage>>,
        event_tx: MTx<mpsc::List<StreamEvent>>,
    ) -> Result<SrtServer> {
        let host = self.srt_config.host.clone();
        let cancel_token = self.cancel_token.child_token();

        let port_allocator = match self.srt_config.srt_port_range() {
            Ok((start, end)) => PortAllocator::new(start, end),
            Err(e) => {
                error!("Failed to create port allocator: {}", e);
                anyhow::bail!("Failed to create port allocator: {}", e);
            }
        };

        let server = SrtServer::new(rx, event_tx, host, port_allocator, cancel_token);
        Ok(server)
    }

    pub async fn spawn_task(self) -> Result<TransportController> {
        let (rtmp_msg_tx, rtmp_msg_rx) = spsc::unbounded_async();
        let (srt_msg_tx, srt_msg_rx) = spsc::unbounded_async();
        let (event_tx, event_rx) = mpsc::unbounded_async();

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

        Ok(TransportController::new(
            rtmp_msg_tx,
            srt_msg_tx,
            handle,
            self.cancel_token,
        ))
    }
}

async fn handle_stream_event(event: StreamEvent) -> Result<()> {
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
                dispatcher::singleton()
                    .await
                    .send(SessionEvent::SessionStarted {
                        live_id: live_id.clone(),
                        protocal: match new_state {
                            SessionState::Rtmp(_) => Protocal::Rtmp,
                            SessionState::Srt(_) => Protocal::Srt,
                        },
                    });
            }

            if new_state.is_stopped() {
                dispatcher::singleton()
                    .await
                    .send(SessionEvent::SessionEnded {
                        live_id: live_id.clone(),
                        protocal: match new_state {
                            SessionState::Rtmp(_) => Protocal::Rtmp,
                            SessionState::Srt(_) => Protocal::Srt,
                        },
                    });
            }
            Ok(())
        }
    }
}

async fn event_listener(event_rx: AsyncRx<mpsc::List<StreamEvent>>) -> Result<()> {
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
