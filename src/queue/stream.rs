use std::fmt;

use crossfire::flavor::{Flavor, FlavorMC};
use crossfire::{AsyncRx, MAsyncRx};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tracing::warn;

use crate::telemetry::metrics;

pub enum BroadcastRecv<T> {
    Item(T),
    Lagged(u64),
    Closed,
}

pub struct ChannelStream<R> {
    rx: R,
    queue: &'static str,
    source: &'static str,
    live_id: Option<String>,
}

impl<R> ChannelStream<R> {
    pub(crate) fn new(
        rx: R,
        queue: &'static str,
        source: &'static str,
        live_id: Option<String>,
    ) -> Self {
        Self {
            rx,
            queue,
            source,
            live_id,
        }
    }

    pub fn with_live_id(mut self, live_id: impl Into<String>) -> Self {
        self.live_id = Some(live_id.into());
        self
    }

    fn on_receiver_closed(&self, error: &impl fmt::Display) {
        match &self.live_id {
            Some(live_id) => warn!(
                queue = self.queue,
                source = self.source,
                live_id = %live_id,
                error = %error,
                "Channel stream receiver closed"
            ),
            None => warn!(
                queue = self.queue,
                source = self.source,
                error = %error,
                "Channel stream receiver closed"
            ),
        }
        metrics::get_metrics().record_queue_drop(self.queue, "disconnected");
    }

    fn on_receiver_lagged(&self, skipped: u64) {
        match &self.live_id {
            Some(live_id) => warn!(
                queue = self.queue,
                source = self.source,
                live_id = %live_id,
                skipped = skipped,
                "Channel stream lagged; skipped stale messages"
            ),
            None => warn!(
                queue = self.queue,
                source = self.source,
                skipped = skipped,
                "Channel stream lagged; skipped stale messages"
            ),
        }
        metrics::get_metrics().record_listener_lag(self.source, skipped.max(1));
    }
}

impl<F: Flavor> ChannelStream<AsyncRx<F>> {
    pub async fn recv(&mut self) -> Option<F::Item>
    where
        F::Item: Send + 'static,
    {
        match self.rx.recv().await {
            Ok(item) => Some(item),
            Err(e) => {
                self.on_receiver_closed(&e);
                None
            }
        }
    }

    pub async fn next(&mut self) -> Option<F::Item>
    where
        F::Item: Send + 'static,
    {
        self.recv().await
    }
}

impl<F: Flavor + FlavorMC> ChannelStream<MAsyncRx<F>> {
    #[allow(dead_code)]
    pub async fn recv(&mut self) -> Option<F::Item>
    where
        F::Item: Send + 'static,
    {
        match self.rx.recv().await {
            Ok(item) => Some(item),
            Err(e) => {
                self.on_receiver_closed(&e);
                None
            }
        }
    }

    #[allow(dead_code)]
    pub async fn next(&mut self) -> Option<F::Item>
    where
        F::Item: Send + 'static,
    {
        self.recv().await
    }
}

impl<T: Clone + Send + 'static> ChannelStream<broadcast::Receiver<T>> {
    pub async fn recv(&mut self) -> BroadcastRecv<T> {
        match self.rx.recv().await {
            Ok(item) => BroadcastRecv::Item(item),
            Err(RecvError::Lagged(skipped)) => {
                self.on_receiver_lagged(skipped as u64);
                BroadcastRecv::Lagged(skipped as u64)
            }
            Err(RecvError::Closed) => {
                self.on_receiver_closed(&"broadcast channel closed");
                BroadcastRecv::Closed
            }
        }
    }

    pub async fn next(&mut self) -> Option<T> {
        loop {
            match self.recv().await {
                BroadcastRecv::Item(item) => return Some(item),
                BroadcastRecv::Lagged(_) => continue,
                BroadcastRecv::Closed => return None,
            }
        }
    }
}
