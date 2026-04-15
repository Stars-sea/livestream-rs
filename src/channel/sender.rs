use crossfire::{BlockingTxTrait, MTx, TrySendError, Tx, mpsc, spsc};
use tokio::sync::broadcast;
use tracing::warn;

use crate::channel::{error::SendError, receiver::Receiver};
use crate::telemetry::metrics;

pub struct Sender<T> {
    inner: T,

    queue: String,
    live_id: Option<String>,
}

impl<T> Sender<T> {
    pub(super) fn new(
        inner: T,
        queue: impl Into<String>,
        live_id: Option<impl Into<String>>,
    ) -> Self {
        Self {
            inner,
            queue: queue.into(),
            live_id: live_id.map(|id| id.into()),
        }
    }

    pub fn with_live_id(self, live_id: impl Into<String>) -> Self {
        Self {
            inner: self.inner,
            queue: self.queue.clone(),
            live_id: Some(live_id.into()),
        }
    }
}

fn send_impl<T>(
    name: impl Into<String>,
    sender: &impl BlockingTxTrait<T>,
    item: T,
) -> Result<(), SendError>
where
    T: Send + 'static,
{
    let name = name.into();
    match sender.try_send(item) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => {
            warn!(
                queue = name,
                live_id = "N/A",
                error = "channel full",
                "Sender channel is full, dropping item"
            );
            metrics::get_metrics().record_queue_drop(&name, "full");
            Err(SendError::Full)
        }
        Err(TrySendError::Disconnected(_)) => {
            warn!(
                queue = name,
                live_id = "N/A",
                error = "channel disconnected",
                "Sender stream closed"
            );
            metrics::get_metrics().record_queue_drop(&name, "disconnected");
            Err(SendError::Closed)
        }
    }
}

impl<T> Sender<MTx<mpsc::Array<T>>>
where
    T: Send + 'static,
{
    pub fn send(&self, item: T) -> Result<(), SendError> {
        send_impl(&self.queue, &self.inner, item)
    }
}

impl<T> Sender<Tx<spsc::Array<T>>>
where
    T: Send + 'static,
{
    pub fn send(&self, item: T) -> Result<(), SendError> {
        send_impl(&self.queue, &self.inner, item)
    }
}

impl<T> Sender<broadcast::Sender<T>>
where
    T: Clone + Send + 'static,
{
    pub fn send(&self, item: T) -> Result<(), SendError> {
        match self.inner.send(item) {
            Ok(_) => Ok(()),
            Err(e) => {
                warn!(
                    queue = self.queue,
                    live_id = self.live_id.as_deref().unwrap_or("N/A"),
                    error = %e,
                    "Sender stream closed"
                );
                metrics::get_metrics().record_queue_drop(&self.queue, "disconnected");
                Err(SendError::Closed)
            }
        }
    }

    pub fn subscribe(&self) -> Receiver<broadcast::Receiver<T>> {
        Receiver::new(self.inner.subscribe(), &self.queue, self.live_id.clone())
    }
}

impl<T> Clone for Sender<T>
where
    T: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            queue: self.queue.clone(),
            live_id: self.live_id.clone(),
        }
    }
}
