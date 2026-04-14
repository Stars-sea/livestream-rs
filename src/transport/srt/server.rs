use anyhow::Result;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

use super::connection::SrtConnectionBuilder;
use crate::dispatcher::Protocol;
use crate::infra::PortAllocator;
use crate::pipeline::PipeBus;
use crate::queue::MpscChannel;
use crate::transport::controller::ControlMessage;
use crate::transport::lifecycle::HandlerLifecycle;
use crate::transport::registry::global;
use crate::transport::registry::state::*;

pub struct SrtServer {
    ctrl_channel: MpscChannel<ControlMessage>,

    bus: PipeBus,

    port_allocator: Arc<PortAllocator>,
    port_cache: Arc<DashMap<String, u16>>, // live_id -> port

    cancel_token: CancellationToken,
}

impl SrtServer {
    pub fn new(
        ctrl_channel: MpscChannel<ControlMessage>,
        bus: PipeBus,
        port_allocator: PortAllocator,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            ctrl_channel,
            bus,
            port_allocator: Arc::new(port_allocator),
            port_cache: Arc::new(DashMap::new()),
            cancel_token,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        let mut ctrl_stream = self
            .ctrl_channel
            .subscribe("transport.srt.server.control_rx")
            .map_err(|e| anyhow::anyhow!("Failed to subscribe SRT control channel: {}", e))?;

        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    debug!("RTMP server cancellation requested, shutting down");
                    break;
                }

                msg = ctrl_stream.next() => {
                    if let Some(msg) = msg {
                        if let Err(e) = self.handle_control_message(msg).await {
                            error!(error = %e, "Failed to handle SRT control message");
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn handle_control_message(&mut self, msg: ControlMessage) -> Result<()> {
        match msg {
            ControlMessage::PrecreateStream {
                live_id,
                passphrase,
            } => {
                // Pre-allocate synchronization resources to circumvent lock contention
                // when the crucial stream headers arrive.
                self.bus.prepare_stream(&live_id);
                self.spawn_connection_handler(live_id, passphrase).await
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

    async fn spawn_connection_handler(
        &mut self,
        live_id: String,
        passphrase: Option<String>,
    ) -> Result<()> {
        let cancel_token = self.cancel_token.child_token();

        let port = match self.port_allocator.allocate_safe_port().await {
            Some(port) => port,
            None => {
                error!(live_id = %live_id, "Failed to allocate port for new SRT connection");
                anyhow::bail!("Failed to allocate port for new SRT connection");
            }
        };
        self.port_cache.insert(live_id.clone(), port);

        let lifecycle = HandlerLifecycle::new(live_id.clone(), Protocol::Srt);

        if let Err(e) = lifecycle
            .pending(
                SessionEndpoint::new(Some(port), passphrase.clone()),
                cancel_token.clone(),
            )
            .await
        {
            self.port_cache.remove(&live_id);
            self.port_allocator.release_port(port);
            return Err(e);
        }

        let cleanup_live_id = live_id.clone();
        let cleanup_port_cache = self.port_cache.clone();
        let cleanup_port_allocator = self.port_allocator.clone();
        let cleanup_token = cancel_token.clone();

        tokio::spawn(async move {
            cleanup_token.cancelled().await;
            if let Some((_, port)) = cleanup_port_cache.remove(&cleanup_live_id) {
                cleanup_port_allocator.release_port(port);
                debug!(
                    live_id = %cleanup_live_id,
                    port = port,
                    "Released SRT port after session cancellation"
                );
            }
        });

        let builder = SrtConnectionBuilder::new(
            port,
            live_id,
            passphrase,
            self.bus.clone(),
            Handle::current(),
        );

        spawn_connection_handler(builder, lifecycle, cancel_token)
    }
}

fn spawn_connection_handler(
    builder: SrtConnectionBuilder,
    lifecycle: HandlerLifecycle,
    cancel_token: CancellationToken,
) -> Result<()> {
    std::thread::spawn(move || {
        let _cancel_guard = cancel_token.clone().drop_guard();
        let stream_id = builder.stream_id().to_string();

        let connection = match builder.build(cancel_token) {
            Ok(c) => c,
            Err(e) => {
                error!(stream_id = %stream_id, "Failed to build SRT connection: {:?}", e);
                return;
            }
        };

        if let Err(e) = connection.run(lifecycle) {
            error!(stream_id = %stream_id, "Error in SRT connection handler: {:?}", e);
        }
    });

    Ok(())
}
