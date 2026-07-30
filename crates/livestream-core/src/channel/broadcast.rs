use std::sync::Arc;

use crossfire::{AsyncRx, mpsc};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::channel::error::SendError;

/// MPSC channel receiver backed by crossfire.
pub struct MpscReceiver<T: 'static> {
    inner: AsyncRx<mpsc::Array<T>>,
}

impl<T> MpscReceiver<T> {
    pub(super) fn new(inner: AsyncRx<mpsc::Array<T>>) -> Self {
        Self { inner }
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
    queue: &'static str,
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
                debug!("Broadcast sender: no active receivers");
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
            use broadcast::error::RecvError;
            match self.inner.recv().await {
                Ok(item) => return Some(item),
                Err(RecvError::Lagged(n)) => warn!(
                    skipped = n,
                    "Broadcast receiver lagged, skipped {n} messages"
                ),
                Err(RecvError::Closed) => return None,
            }
        }
    }

    /// Try to receive the next item without blocking. Returns `None` if no
    /// message is immediately available.
    pub fn try_recv(&mut self) -> Option<T>
    where
        T: Clone,
    {
        use broadcast::error::TryRecvError;
        match self.inner.try_recv() {
            Ok(item) => Some(item),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Lagged(n)) => {
                warn!(
                    skipped = n,
                    "Broadcast receiver lagged, skipped {n} messages"
                );
                None
            }
            Err(TryRecvError::Closed) => None,
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
