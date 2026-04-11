use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Result;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, instrument};

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
use crate::queue::{Channel, MpscChannel};
use crate::telemetry::metrics;

pub struct TransportServer {
    rtmp_config: RtmpConfig,
    srt_config: SrtConfig,
    queue_config: QueueConfig,
    rtmp_tag_channel: Option<MpscChannel<StreamFlvTag>>,

    bus: PipeBus,

    cancel_token: CancellationToken,
}

impl TransportServer {
    pub fn new(
        rtmp_config: RtmpConfig,
        srt_config: SrtConfig,
        queue_config: QueueConfig,
        rtmp_tag_channel: MpscChannel<StreamFlvTag>,
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

    async fn rtmp_server(
        &mut self,
        control_channel: MpscChannel<ControlMessage>,
        event_channel: MpscChannel<StreamEvent>,
    ) -> Result<RtmpServer> {
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
            event_channel,
            rtmp_tag_channel,
            self.bus.clone(),
            cancel_token,
        )
        .await?;
        Ok(server)
    }

    async fn srt_server(
        &self,
        control_channel: MpscChannel<ControlMessage>,
        event_channel: MpscChannel<StreamEvent>,
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
            control_channel,
            event_channel,
            self.bus.clone(),
            port_allocator,
            cancel_token,
        );
        Ok(server)
    }

    pub async fn spawn_task(mut self) -> Result<(TransportController, JoinHandle<Result<()>>)> {
        let rtmp_control_channel = Channel::mpsc_bounded(
            "control_rtmp",
            "transport.server.rtmp_control",
            self.queue_config.control,
        );
        let srt_control_channel = Channel::mpsc_bounded(
            "control_srt",
            "transport.server.srt_control",
            self.queue_config.control,
        );
        let event_channel = Channel::mpsc_bounded(
            "transport_event",
            "transport.server.event",
            self.queue_config.event,
        );

        let rtmp_server = self
            .rtmp_server(rtmp_control_channel.clone(), event_channel.clone())
            .await?;
        let srt_server = self
            .srt_server(srt_control_channel.clone(), event_channel.clone())
            .await?;

        let handle = tokio::spawn(async move {
            tokio::try_join!(
                rtmp_server.run(),
                srt_server.run(),
                event_listener(event_channel)
            )?;
            Ok(())
        });

        let controller = TransportController::new(
            rtmp_control_channel.with_source("transport.controller.rtmp_tx"),
            srt_control_channel.with_source("transport.controller.srt_tx"),
        );
        Ok((controller, handle))
    }
}

#[instrument(
    name = "transport.stream_event",
    skip(event),
    fields(
        event.kind = %event.kind(),
        live_id = %event.live_id(),
    )
)]
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
                if let Some(ct) = global::get_cancel_token(&live_id).await {
                    if !ct.is_cancelled() {
                        ct.cancel();

                        let protocol = match new_state {
                            SessionState::Rtmp(_) => "rtmp",
                            SessionState::Srt(_) => "srt",
                        };
                        metrics::get_metrics().record_auto_recycle(protocol, "disconnected_state");

                        debug!(
                            live_id = %live_id,
                            protocol = protocol,
                            "Auto-cancelled session after disconnected state"
                        );
                    }
                }

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

async fn event_listener(event_channel: MpscChannel<StreamEvent>) -> Result<()> {
    let mut events = event_channel
        .subscribe("transport.server.event_listener")
        .map_err(|e| anyhow::anyhow!("Failed to subscribe transport event listener: {}", e))?;

    while let Some(event) = events.next().await {
        if let Err(e) = handle_stream_event(event).await {
            error!(error = %e, "Failed to handle stream event");
        }
    }
    Ok(())
}
