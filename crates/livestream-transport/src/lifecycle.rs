use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::dispatcher::{self, SessionEvent};
use crate::registry;
use crate::registry::state::{SessionDescriptor, SessionEndpoint, SessionState};
use livestream_core::types::Protocol;
use livestream_media::stream::StreamCollection;

pub struct HandlerLifecycle {
    live_id: String,
    protocol: Protocol,

    initialized: AtomicBool,
    connected: AtomicBool,
    disconnected: AtomicBool,
}

impl HandlerLifecycle {
    pub fn new(live_id: String, protocol: Protocol) -> Self {
        Self {
            live_id,
            protocol,
            initialized: AtomicBool::new(false),
            connected: AtomicBool::new(false),
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

    pub async fn init(&self, _streams: Arc<dyn StreamCollection + Send + Sync + 'static>) {
        if self.initialized() {
            return;
        }

        self.initialized.store(true, Ordering::Relaxed);

        // NOTE: Pipeline engine is a stub in Phase 6. SessionInit events are
        // dispatched but not consumed by any pipeline factory.
        dispatcher::INSTANCE.send(SessionEvent::SessionInit {
            live_id: self.live_id.clone(),
            streams: _streams,
        });
    }

    pub async fn connecting(&self) -> Result<()> {
        registry::INSTANCE
            .update_state(&self.live_id, SessionState::Connecting)
            .await
    }

    pub async fn connect(&self) -> Result<()> {
        // Idempotency guard: dispatch SessionStarted only once per lifecycle.
        if self
            .connected
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }

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

        let Some(ct) = registry::INSTANCE.get_cancel_token(&self.live_id) else {
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
        self.disconnect();
    }
}
