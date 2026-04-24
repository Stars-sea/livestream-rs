use anyhow::Result;
use crossfire::{AsyncRx, AsyncRxTrait, mpsc, spsc};
use tokio::sync::broadcast;
use tracing::warn;

use crate::{channel::error::RecvError, metric_listener_lag, metric_queue_drop};
use std::sync::Arc;

pub struct Receiver<T> {
    inner: T,

    queue: &'static str,
    live_id: Option<Arc<str>>,
}

impl<T> Receiver<T> {
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

async fn recv_impl<T>(
    queue: &'static str,
    live_id: Option<Arc<str>>,
    receiver: &mut impl AsyncRxTrait<T>,
) -> Result<T, RecvError>
where
    T: Send + 'static,
{
    match receiver.recv().await {
        Ok(res) => Ok(res),
        Err(e) => {
            warn!(
                queue = queue,
                live_id = %live_id.as_deref().unwrap_or("N/A"),
                error = %e,
                "Receiver stream closed"
            );
            metric_queue_drop!(queue, "disconnected");
            Err(RecvError::Closed)
        }
    }
}

macro_rules! impl_async_receiver {
    ($inner:ty) => {
        impl<T> Receiver<$inner>
        where
            T: Send + 'static,
        {
            pub async fn recv(&mut self) -> Result<T, RecvError> {
                recv_impl(self.queue, self.live_id.clone(), &mut self.inner).await
            }

            pub async fn next(&mut self) -> Option<T> {
                self.recv().await.ok()
            }
        }
    };
}

impl_async_receiver!(AsyncRx<mpsc::Array<T>>);
impl_async_receiver!(AsyncRx<spsc::Array<T>>);

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
                    live_id = %self.live_id.as_deref().unwrap_or("N/A"),
                    skipped = n,
                    "Receiver lagged and skipped {n} messages",
                );
                metric_listener_lag!(self.queue, n);
                Err(RecvError::Lagged(n))
            }
            Err(broadcast::error::RecvError::Closed) => {
                warn!(
                    queue = self.queue,
                    live_id = %self.live_id.as_deref().unwrap_or("N/A"),
                    error = "Stream closed",
                    "Receiver stream closed"
                );
                metric_queue_drop!(self.queue, "disconnected");
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
            queue: self.queue,
            live_id: self.live_id.clone(),
        }
    }
}
