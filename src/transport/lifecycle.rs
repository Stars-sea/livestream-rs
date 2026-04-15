use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::dispatcher::{self, Protocol, SessionEvent};
use crate::infra::media::stream::StreamCollection;
use crate::transport::registry::state::{SessionDescriptor, SessionEndpoint};
use crate::transport::registry::{global, state::SessionState};

pub struct HandlerLifecycle {
    live_id: String,
    protocol: Protocol,

    initialized: AtomicBool,
    disconnected: AtomicBool,
}

impl HandlerLifecycle {
    pub fn new(live_id: String, protocol: Protocol) -> Self {
        Self {
            live_id,
            protocol,
            initialized: AtomicBool::new(false),
            disconnected: AtomicBool::new(false),
        }
    }

    pub fn initialized(&self) -> bool {
        self.initialized.load(Ordering::Relaxed)
    }

    pub async fn state(&self) -> Result<SessionState> {
        global::get_session_state(&self.live_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("Session state not found for live_id: {}", self.live_id))
    }

    pub async fn pending(
        &self,
        endpoint: SessionEndpoint,
        cancel_token: CancellationToken,
    ) -> Result<()> {
        global::register_session(
            SessionDescriptor {
                id: self.live_id.clone(),
                protocol: self.protocol,
                endpoint,
                state: SessionState::Pending,
            },
            cancel_token,
        )
        .await
    }

    pub async fn init(&self, streams: Arc<dyn StreamCollection + Send + Sync + 'static>) {
        if self.initialized() {
            return;
        }

        self.initialized.store(true, Ordering::Relaxed);

        dispatcher::singleton()
            .await
            .send(SessionEvent::SessionInit {
                live_id: self.live_id.clone(),
                streams: streams,
            });
    }

    pub async fn connecting(&self) -> Result<()> {
        global::update_session_state(&self.live_id, SessionState::Connecting).await
    }

    pub async fn connected(&self) -> Result<()> {
        global::update_session_state(&self.live_id, SessionState::Connected).await?;

        dispatcher::singleton()
            .await
            .send(SessionEvent::SessionStarted {
                live_id: self.live_id.clone(),
                protocol: self.protocol,
            });
        Ok(())
    }

    pub async fn disconnected(&self) -> Result<()> {
        if !self.try_mark_disconnected() {
            return Ok(());
        }

        Self::run_disconnected(&self.live_id, self.protocol).await
    }

    fn try_mark_disconnected(&self) -> bool {
        self.disconnected
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    async fn run_disconnected(live_id: &str, protocol: Protocol) -> Result<()> {
        global::update_session_state(live_id, SessionState::Disconnected).await?;
        let Some(ct) = global::get_cancel_token(live_id).await else {
            // This can happen if the session was never fully registered or if it was already cleaned up
            debug!(live_id = %live_id, "No cancellation token found for live_id during disconnect");
            return Ok(());
        };
        ct.cancel();

        dispatcher::singleton()
            .await
            .send(SessionEvent::SessionEnded {
                live_id: live_id.to_string(),
                protocol,
            });

        Ok(())
    }
}

impl Drop for HandlerLifecycle {
    fn drop(&mut self) {
        if !self.try_mark_disconnected() {
            return;
        }

        let live_id = self.live_id.clone();
        let protocol = self.protocol;

        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            debug!(
                live_id = %live_id,
                "No Tokio runtime available in Drop; skip async disconnected fallback"
            );
            return;
        };

        handle.spawn(async move {
            if let Err(e) = Self::run_disconnected(&live_id, protocol).await {
                debug!(
                    live_id = %live_id,
                    error = %e,
                    "Drop fallback failed to mark session disconnected"
                );
            }
        });
    }
}
