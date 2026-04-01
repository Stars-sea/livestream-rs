use anyhow::Result;
use crossfire::{AsyncRx, MTx, mpsc, spsc};
use dashmap::DashMap;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

use super::connection::SrtConnectionBuilder;
use crate::infra::PortAllocator;
use crate::pipeline::{PipeBus, UnifiedPacketContext};
use crate::transport::contract::message::{ControlMessage, StreamEvent};
use crate::transport::contract::state::{SessionDescriptor, SessionState, SrtState};
use crate::transport::registry::global;
use crate::transport::srt::packet::WrappedPacket;

pub struct SrtServer {
    ctrl_rx: AsyncRx<spsc::List<ControlMessage>>,
    event_tx: MTx<mpsc::List<StreamEvent>>,
    packet_rx: AsyncRx<mpsc::List<WrappedPacket>>,
    packet_tx: MTx<mpsc::List<WrappedPacket>>,

    bus: PipeBus,

    host: String,
    port_allocator: PortAllocator,
    port_cache: DashMap<String, u16>, // live_id -> port

    cancel_token: CancellationToken,
}

impl SrtServer {
    pub fn new(
        ctrl_rx: AsyncRx<spsc::List<ControlMessage>>,
        event_tx: MTx<mpsc::List<StreamEvent>>,
        bus: PipeBus,
        host: String,
        port_allocator: PortAllocator,
        cancel_token: CancellationToken,
    ) -> Self {
        let (packet_tx, packet_rx) = mpsc::unbounded_async();
        Self {
            ctrl_rx,
            event_tx,
            packet_rx,
            packet_tx,
            bus,
            host,
            port_allocator,
            port_cache: DashMap::new(),
            cancel_token,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    debug!("RTMP server cancellation requested, shutting down");
                    break;
                }

                msg = self.ctrl_rx.recv() => {
                    match msg {
                        Ok(msg) => self.handle_control_message(msg).await?,
                        Err(e) => debug!("Error receiving control message: {:?}", e),
                    }
                }

                packet = self.packet_rx.recv() => {
                    match packet {
                        Ok(packet) => {
                            if let Err(e) = self.handle_packet_received(packet).await {
                                debug!("Error handling received packet: {:?}", e);
                            }
                        }
                        Err(e) => debug!("Error receiving packet: {:?}", e),
                    }
                }
            }
        }

        Ok(())
    }

    async fn handle_control_message(&mut self, msg: ControlMessage) -> Result<()> {
        match msg {
            ControlMessage::PrecreateStream { live_id } => {
                self.spawn_connection_handler(live_id).await
            }
            ControlMessage::StopStream { live_id } => {
                if let Some(cancel_token) = global::get_cancel_token(&live_id).await {
                    cancel_token.cancel();
                }

                let port = match self.port_cache.remove(&live_id) {
                    Some((_, port)) => port,
                    None => {
                        debug!(live_id = %live_id, "No port found for live_id in cache during StopStream");
                        return Ok(()); // No port to release, just return
                    }
                };
                self.port_allocator.release_port(port);

                Ok(())
            }
        }
    }

    async fn handle_packet_received(&mut self, packet: WrappedPacket) -> Result<()> {
        let WrappedPacket {
            stream_id,
            packet,
            cancel_token,
        } = packet;

        // TODO: Send UnifiedPacket::Init
        let context = UnifiedPacketContext::new(stream_id.clone(), packet.into(), cancel_token);
        self.bus.send_packet(context).await?;
        Ok(())
    }

    async fn spawn_connection_handler(&mut self, live_id: String) -> Result<()> {
        let cancel_token = self.cancel_token.child_token();

        let port = match self.port_allocator.allocate_safe_port().await {
            Some(port) => port,
            None => {
                error!(live_id = %live_id, "Failed to allocate port for new SRT connection");
                anyhow::bail!("Failed to allocate port for new SRT connection");
            }
        };
        self.port_cache.insert(live_id.clone(), port);

        let session = SessionDescriptor {
            id: live_id.clone(),
            state: SessionState::Srt(SrtState::Pending),
        };
        global::register_session(session, cancel_token.clone()).await?;

        let builder = SrtConnectionBuilder::new(
            self.host.clone(),
            port,
            live_id,
            "passphrase".to_string(), // TODO
            self.packet_tx.clone(),
            self.event_tx.clone(),
        );

        spawn_connection_handler(builder, cancel_token)
    }
}

fn spawn_connection_handler(
    builder: SrtConnectionBuilder,
    cancel_token: CancellationToken,
) -> Result<()> {
    let _cancel_guard = cancel_token.clone().drop_guard();

    let live_id = builder.live_id().to_string();
    let connection = builder.build(cancel_token)?;

    std::thread::spawn(move || {
        let _cancel_guard = _cancel_guard;
        if let Err(e) = connection.run() {
            error!(live_id = %live_id, "Error in SRT connection handler: {:?}", e);
        }
    });

    Ok(())
}
