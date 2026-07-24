use std::sync::Arc;

use crossfire::{AsyncRx, mpsc};
use tokio::sync::broadcast;
use tracing::warn;

use crate::channel::error::SendError;

/// MPSC channel receiver backed by crossfire.
pub struct MpscReceiver<T: 'static> {
    inner: AsyncRx<mpsc::Array<T>>,
    #[allow(dead_code)]
    queue: &'static str,
    #[allow(dead_code)]
    live_id: Option<Arc<str>>,
}

impl<T> MpscReceiver<T> {
    pub(super) fn new(
        inner: AsyncRx<mpsc::Array<T>>,
        queue: &'static str,
        live_id: Option<Arc<str>>,
    ) -> Self {
        Self {
            inner,
            queue,
            live_id,
        }
    }

    /// Receive the next item (async). Returns None if all senders dropped.
    pub async fn recv(&mut self) -> Option<T>
    where
        T: Send + 'static,
    {
        match self.inner.recv().await {
            Ok(item) => Some(item),
            Err(e) => {
                warn!(
                    error = %e,
                    "MPSC receiver: channel closed"
                );
                None
            }
        }
    }
}

/// Broadcast channel sender (tokio::sync::broadcast).
pub struct BroadcastSender<T> {
    inner: broadcast::Sender<T>,
    #[allow(dead_code)]
    queue: &'static str,
    #[allow(dead_code)]
    live_id: Option<Arc<str>>,
}

impl<T> BroadcastSender<T> {
    pub(super) fn new(
        inner: broadcast::Sender<T>,
        queue: &'static str,
        live_id: Option<Arc<str>>,
    ) -> Self {
        Self {
            inner,
            queue,
            live_id,
        }
    }

    /// Send to all subscribers (non-blocking).
    pub fn send(&self, item: T) -> Result<usize, SendError>
    where
        T: Clone,
    {
        match self.inner.send(item) {
            Ok(n) => Ok(n),
            Err(_) => {
                // tokio broadcast returns Err only when all receivers are gone
                warn!("Broadcast sender: no active receivers");
                Err(SendError::Closed)
            }
        }
    }

    pub fn subscribe(&self) -> BroadcastReceiver<T> {
        BroadcastReceiver::new(self.inner.subscribe())
    }
}

impl<T> Clone for BroadcastSender<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            queue: self.queue,
            live_id: self.live_id.clone(),
        }
    }
}

/// Broadcast channel receiver (tokio::sync::broadcast).
pub struct BroadcastReceiver<T> {
    inner: broadcast::Receiver<T>,
}

impl<T> BroadcastReceiver<T> {
    pub(super) fn new(inner: broadcast::Receiver<T>) -> Self {
        Self { inner }
    }

    /// Receive the next item. Skips lagged messages automatically.
    pub async fn recv(&mut self) -> Option<T>
    where
        T: Clone,
    {
        loop {
            match self.inner.recv().await {
                Ok(item) => return Some(item),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        skipped = n,
                        "Broadcast receiver lagged, skipped {n} messages"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return None;
                }
            }
        }
    }
}

impl<T: Clone> Clone for BroadcastReceiver<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.resubscribe(),
        }
    }
}
