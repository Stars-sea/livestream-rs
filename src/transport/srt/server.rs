use anyhow::Result;
use crossfire::{AsyncRx, MTx, mpsc, spsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

use super::connection::SrtConnectionBuilder;
use crate::infra::PortAllocator;
use crate::transport::contract::message::{ControlMessage, StreamEvent};
use crate::transport::contract::state::{SessionDescriptor, SessionState, SrtState};
use crate::transport::registry::global;

pub struct SrtServer {
    ctrl_rx: AsyncRx<spsc::List<ControlMessage>>,
    event_tx: MTx<mpsc::List<StreamEvent>>,

    host: String,
    port_allocator: PortAllocator,

    cancel_token: CancellationToken,
}

impl SrtServer {
    pub fn new(
        ctrl_rx: AsyncRx<spsc::List<ControlMessage>>,
        event_tx: MTx<mpsc::List<StreamEvent>>,
        host: String,
        port_allocator: PortAllocator,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            ctrl_rx,
            event_tx,
            host,
            port_allocator,
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

                Ok(())
            }
        }
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
            self.event_tx.clone(),
        );

        spawn_connection_handler(builder, cancel_token);
        Ok(())
    }
}

fn spawn_connection_handler(
    builder: SrtConnectionBuilder,
    cancel_token: CancellationToken,
) -> Result<()> {
    let _cancel_guard = cancel_token.clone().drop_guard();

    let connection = builder.build(cancel_token)?;

    std::thread::spawn(move || {
        let _cancel_guard = _cancel_guard;
        connection.run()
    });

    Ok(())
}
