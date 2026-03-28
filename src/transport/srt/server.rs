use std::sync::Arc;

use anyhow::{Result, anyhow};
use crossfire::{AsyncRx, MTx, mpsc, spsc};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

use super::SrtConnection;
use crate::transport::message::{ControlMessage, StreamEvent};
use crate::transport::{SessionDescriptor, SessionState, SrtState, global};

pub struct SrtServer {
    ctrl_rx: AsyncRx<spsc::List<ControlMessage>>,
    event_rx: AsyncRx<mpsc::List<StreamEvent>>,

    event_tx: MTx<mpsc::List<StreamEvent>>,

    cancel_token: CancellationToken,
}

impl SrtServer {
    pub fn new(
        ctrl_rx: AsyncRx<spsc::List<ControlMessage>>,
        cancel_token: CancellationToken,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_async();
        Self {
            ctrl_rx,
            event_rx,
            event_tx,

            cancel_token,
        }
    }

    pub async fn run(&self) -> Result<()> {
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

                event = self.event_rx.recv() => {
                    match event {
                        Ok(event) => self.handle_stream_event(event).await?,
                        Err(e) => debug!("Error receiving stream event: {:?}", e),
                    }
                }
            }
        }

        Ok(())
    }

    async fn handle_control_message(&self, msg: ControlMessage) -> Result<()> {
        match msg {
            ControlMessage::PrecreateStream { live_id } => {
                let event_tx = self.event_tx.clone();
                let cancel_token = self.cancel_token.child_token();
                spawn_connection_handler(live_id, event_tx, cancel_token).await
            }
            ControlMessage::StopStream { live_id } => {
                if let Some(cancel_token) = global::get_cancel_token(&live_id).await {
                    cancel_token.cancel();
                }

                Ok(())
            }
        }
    }

    async fn handle_stream_event(&self, event: StreamEvent) -> Result<()> {
        match event {
            StreamEvent::StateChange { live_id, new_state } => {
                debug!(live_id = %live_id, new_state = ?new_state, "Stream state changed");
                if let Err(e) = global::update_session_state(&live_id, new_state).await {
                    error!(error = %e, live_id = %live_id, "Failed to update session state, cancelling stream");
                    let cancel_token = global::get_cancel_token(&live_id).await.ok_or(anyhow!(
                        "No cancellation token found for live_id: {}",
                        live_id
                    ))?;
                    cancel_token.cancel();
                }
                Ok(())
            }
        }
    }
}

async fn spawn_connection_handler(
    live_id: String,
    event_tx: MTx<mpsc::List<StreamEvent>>,
    cancel_token: CancellationToken,
) -> Result<()> {
    let connection = SrtConnection::new(live_id.clone(), event_tx, cancel_token.clone());

    let session = SessionDescriptor {
        id: live_id.clone(),
        state: SessionState::Srt(SrtState::Pending),
    };
    let session = Arc::new(RwLock::new(session));

    // TODO:
    // Solve the synchronization issue here. If the connection handler fails to register the session or cancellation token,
    // we need to ensure that any partial state is cleaned up properly.
    let res1 = global::register_session(session.clone()).await;
    let res2 = global::register_cancel_token(&live_id, cancel_token.clone()).await;
    if res1.is_err() || res2.is_err() {
        error!(live_id = %live_id, "Failed to register session or cancellation token");
        cancel_token.cancel();
        global::remove_session(session).await;

        anyhow::bail!("Failed to register session or cancellation token");
    }

    std::thread::spawn(move || {
        let _cancel_guard = cancel_token.drop_guard();
        connection.run()
    });

    Ok(())
}
