use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::dispatcher::{EndReason, EventDispatcher, SessionEvent};
use crate::registry::{
    SessionRegistry,
    state::{SessionDescriptor, SessionEndpoint, SessionState},
};
use livestream_core::types::Protocol;
use livestream_media::stream::StreamCollection;

pub struct HandlerLifecycle {
    live_id: String,
    protocol: Protocol,
    registry: Arc<SessionRegistry>,
    dispatcher: Arc<EventDispatcher>,

    initialized: AtomicBool,
    connected: AtomicBool,
    disconnected: AtomicBool,
}

impl HandlerLifecycle {
    pub fn new(
        live_id: String,
        protocol: Protocol,
        registry: Arc<SessionRegistry>,
        dispatcher: Arc<EventDispatcher>,
    ) -> Self {
        Self {
            live_id,
            protocol,
            registry,
            dispatcher,
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
        self.registry
            .register_session(Arc::new(RwLock::new(descriptor)), cancel_token)
            .await
    }

    pub async fn init(&self, _streams: Arc<dyn StreamCollection + Send + Sync + 'static>) {
        if self.initialized() {
            return;
        }

        self.initialized.store(true, Ordering::Relaxed);

        self.dispatcher.send(SessionEvent::SessionInit {
            live_id: self.live_id.clone(),
            streams: _streams,
        });
    }

    pub async fn connecting(&self) -> Result<()> {
        self.registry
            .update_state(&self.live_id, SessionState::Connecting)
            .await
    }

    pub async fn connect(&self) -> Result<()> {
        if self
            .connected
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }

        self.registry
            .update_state(&self.live_id, SessionState::Connected)
            .await?;

        self.dispatcher.send(SessionEvent::SessionStarted {
            live_id: self.live_id.clone(),
            protocol: self.protocol,
        });
        Ok(())
    }

    pub fn disconnect(&self) {
        self.disconnect_with_reason(EndReason::ClientDisconnect);
    }

    pub fn disconnect_with_reason(&self, reason: EndReason) {
        if !self.try_mark_disconnected() {
            return;
        }

        let Some(ct) = self.registry.get_cancel_token(&self.live_id) else {
            debug!(live_id = %self.live_id, "No cancellation token found for live_id during disconnect");
            return;
        };
        ct.cancel();

        self.dispatcher.send(SessionEvent::SessionEnded {
            live_id: self.live_id.clone(),
            protocol: self.protocol,
            reason,
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
        if !self.disconnected.load(Ordering::Acquire) {
            self.disconnect();
        }
    }
}
