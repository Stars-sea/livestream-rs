use anyhow::Result;
use crossfire::{AsyncRx, AsyncRxTrait, mpsc, spsc};
use tokio::sync::broadcast;
use tracing::warn;

use crate::{channel::error::RecvError, telemetry::metrics};

pub struct Receiver<T> {
    inner: T,

    queue: String,
    live_id: Option<String>,
}

impl<T> Receiver<T> {
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

    pub fn with_live_id(mut self, live_id: impl Into<String>) -> Self {
        self.live_id = Some(live_id.into());
        self
    }
}

async fn recv_impl<T>(
    name: impl Into<String>,
    receiver: &mut impl AsyncRxTrait<T>,
) -> Result<T, RecvError>
where
    T: Send + 'static,
{
    let name = name.into();
    match receiver.recv().await {
        Ok(res) => Ok(res),
        Err(e) => {
            warn!(
                queue = name,
                live_id = "N/A",
                error = %e,
                "Receiver stream closed"
            );
            metrics::get_metrics().record_queue_drop(&name, "disconnected");
            Err(RecvError::Closed)
        }
    }
}

impl<T> Receiver<AsyncRx<mpsc::Array<T>>>
where
    T: Send + 'static,
{
    pub async fn recv(&mut self) -> Result<T, RecvError> {
        recv_impl(&self.queue, &mut self.inner).await
    }

    pub async fn next(&mut self) -> Option<T> {
        match self.recv().await {
            Ok(item) => Some(item),
            Err(_) => None,
        }
    }
}

impl<T> Receiver<AsyncRx<spsc::Array<T>>>
where
    T: Send + 'static,
{
    pub async fn recv(&mut self) -> Result<T, RecvError> {
        recv_impl(&self.queue, &mut self.inner).await
    }

    pub async fn next(&mut self) -> Option<T> {
        match self.recv().await {
            Ok(item) => Some(item),
            Err(_) => None,
        }
    }
}

impl<T> Receiver<broadcast::Receiver<T>>
where
    T: Clone + Send + 'static,
{
    pub async fn recv(&mut self) -> Result<T, RecvError> {
        match self.inner.recv().await {
            Ok(res) => Ok(res),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!(
                    queue = self.queue,
                    live_id = self.live_id.as_deref().unwrap_or("N/A"),
                    skipped = n,
                    "Receiver lagged and skipped {n} messages",
                );
                metrics::get_metrics().record_listener_lag(&self.queue, n);
                Err(RecvError::Lagged(n))
            }
            Err(broadcast::error::RecvError::Closed) => {
                warn!(
                    queue = self.queue,
                    live_id = self.live_id.as_deref().unwrap_or("N/A"),
                    error = "Stream closed",
                    "Receiver stream closed"
                );
                metrics::get_metrics().record_queue_drop(&self.queue, "disconnected");
                Err(RecvError::Closed)
            }
        }
    }

    pub async fn next(&mut self) -> Option<T> {
        loop {
            match self.recv().await {
                Ok(item) => return Some(item),
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return None,
            }
        }
    }
}

impl<T> Clone for Receiver<T>
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
