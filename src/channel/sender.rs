use crossfire::{BlockingTxTrait, MTx, TrySendError, Tx, mpsc, spsc};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::warn;

use crate::channel::{error::SendError, receiver::Receiver};
use crate::metric_queue_drop;

pub struct Sender<T> {
    inner: T,

    queue: &'static str,
    live_id: Option<Arc<str>>,
}

impl<T> Sender<T> {
    pub(super) fn new(inner: T, queue: &'static str, live_id: Option<Arc<str>>) -> Self {
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
}

fn send_impl<T>(
    queue: &'static str,
    live_id: &Option<Arc<str>>,
    sender: &impl BlockingTxTrait<T>,
    item: T,
) -> Result<(), SendError>
where
    T: Send + 'static,
{
    match sender.try_send(item) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => {
            warn!(
                queue = queue,
                live_id = %live_id.as_deref().unwrap_or("N/A"),
                error = "channel full",
                "Sender channel is full, dropping item"
            );
            metric_queue_drop!(queue, "full");
            Err(SendError::Full)
        }
        Err(TrySendError::Disconnected(_)) => {
            warn!(
                queue = queue,
                live_id = %live_id.as_deref().unwrap_or("N/A"),
                error = "channel disconnected",
                "Sender stream closed"
            );
            metric_queue_drop!(queue, "disconnected");
            Err(SendError::Closed)
        }
    }
}

impl<T> Sender<MTx<mpsc::Array<T>>>
where
    T: Send + 'static,
{
    pub fn send(&self, item: T) -> Result<(), SendError> {
        send_impl(self.queue, &self.live_id, &self.inner, item)
    }
}

impl<T> Sender<Tx<spsc::Array<T>>>
where
    T: Send + 'static,
{
    pub fn send(&self, item: T) -> Result<(), SendError> {
        send_impl(self.queue, &self.live_id, &self.inner, item)
    }
}

impl<T> Sender<broadcast::Sender<T>>
where
    T: Clone + Send + 'static,
{
    pub fn send(&self, item: T) -> Result<usize, SendError> {
        match self.inner.send(item) {
            Ok(n) => Ok(n),
            Err(_) if self.inner.is_empty() => Ok(0),
            Err(e) => {
                warn!(
                    queue = self.queue,
                    live_id = %self.live_id.as_deref().unwrap_or("N/A"),
                    error = %e,
                    "Sender stream closed"
                );
                metric_queue_drop!(self.queue, "disconnected");
                Err(SendError::Closed)
            }
        }
    }

    pub fn subscribe(&self) -> Receiver<broadcast::Receiver<T>> {
        Receiver::new(self.inner.subscribe(), self.queue, self.live_id.clone())
    }
}

impl<T> Clone for Sender<T>
where
    T: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            queue: self.queue,
            live_id: self.live_id.clone(),
        }
    }
}
