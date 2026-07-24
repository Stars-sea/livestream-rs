use std::sync::Arc;

use crossfire::{MTx, TrySendError, mpsc};
use tracing::warn;

use crate::channel::error::SendError;

/// MPSC channel sender backed by crossfire.
pub struct MpscSender<T: 'static> {
    inner: MTx<mpsc::Array<T>>,
    queue: &'static str,
    live_id: Option<Arc<str>>,
}

impl<T> MpscSender<T> {
    pub(super) fn new(
        inner: MTx<mpsc::Array<T>>,
        queue: &'static str,
        live_id: Option<Arc<str>>,
    ) -> Self {
        Self {
            inner,
            queue,
            live_id,
        }
    }

    pub fn with_live_id(mut self, live_id: impl Into<Arc<str>>) -> Self {
        self.live_id = Some(live_id.into());
        self
    }

    /// Send an item. Returns `Full` when the channel is at capacity so the
    /// caller can apply backpressure or retry.
    pub fn send(&self, item: T) -> Result<(), SendError>
    where
        T: Send + 'static,
    {
        match self.inner.try_send(item) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_item)) => {
                warn!(
                    queue = self.queue,
                    live_id = %self.live_id.as_deref().unwrap_or("N/A"),
                    "MPSC sender: channel full, item dropped"
                );
                Err(SendError::Full)
            }
            Err(TrySendError::Disconnected(_)) => {
                warn!(
                    queue = self.queue,
                    live_id = %self.live_id.as_deref().unwrap_or("N/A"),
                    "MPSC sender: channel disconnected"
                );
                Err(SendError::Closed)
            }
        }
    }
}

impl<T: 'static> Clone for MpscSender<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            queue: self.queue,
            live_id: self.live_id.clone(),
        }
    }
}
