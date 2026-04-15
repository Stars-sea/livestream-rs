use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::dispatcher::{self, Protocol, SessionEvent};
use crate::infra::media::stream::StreamCollection;
use crate::transport::registry;
use crate::transport::registry::state::{SessionDescriptor, SessionEndpoint, SessionState};

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

    pub fn disconnected(&self) -> bool {
        self.disconnected.load(Ordering::Relaxed)
    }

    pub async fn pending(
        &self,
        endpoint: SessionEndpoint,
        cancel_token: CancellationToken,
    ) -> Result<()> {
        if self.disconnected() {
            anyhow::bail!(
                "Cannot register pending session for live_id {} because it is already marked as disconnected",
                self.live_id
            );
        }

        let descriptor = SessionDescriptor {
            id: self.live_id.clone(),
            protocol: self.protocol,
            endpoint,
            state: SessionState::Pending,
        };
        registry::INSTANCE
            .register_session(Arc::new(RwLock::new(descriptor)), cancel_token)
            .await
    }

    pub async fn init(&self, streams: Arc<dyn StreamCollection + Send + Sync + 'static>) {
        if self.initialized() {
            return;
        }

        self.initialized.store(true, Ordering::Relaxed);

        dispatcher::INSTANCE.send(SessionEvent::SessionInit {
            live_id: self.live_id.clone(),
            streams: streams,
        });
    }

    pub async fn connecting(&self) -> Result<()> {
        registry::INSTANCE
            .update_state(&self.live_id, SessionState::Connecting)
            .await
    }

    pub async fn connect(&self) -> Result<()> {
        registry::INSTANCE
            .update_state(&self.live_id, SessionState::Connected)
            .await?;

        dispatcher::INSTANCE.send(SessionEvent::SessionStarted {
            live_id: self.live_id.clone(),
            protocol: self.protocol,
        });
        Ok(())
    }

    pub fn disconnect(&self) {
        if !self.try_mark_disconnected() {
            return;
        }

        // Cancel this session's cancellation token will trigger cleanup of all associated resources
        // and will also set state to Disconnected for a period before the session is fully removed from the registry.
        // See: ConnectionRegistry::register_session for details.
        let Some(ct) = registry::INSTANCE.get_cancel_token(&self.live_id) else {
            // This can happen if the session was never fully registered or if it was already cleaned up
            debug!(live_id = %self.live_id, "No cancellation token found for live_id during disconnect");
            return;
        };
        ct.cancel();

        dispatcher::INSTANCE.send(SessionEvent::SessionEnded {
            live_id: self.live_id.clone(),
            protocol: self.protocol,
        });
    }

    fn try_mark_disconnected(&self) -> bool {
        self.disconnected
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

impl Drop for HandlerLifecycle {
    fn drop(&mut self) {
        if !self.try_mark_disconnected() {
            return;
        }

        self.disconnect();
    }
}
