use std::sync::{Arc, Mutex};

use crossfire::flavor::{Flavor, FlavorMC};
use crossfire::{AsyncRx, MAsyncRx, MTx, Tx, mpmc, mpsc, spsc};
use tokio::sync::broadcast;
use tracing::warn;

use crate::telemetry::metrics;

use super::error::SubscribeError;
use super::send::{ChannelTx, SendError};
use super::stream::ChannelStream;
use super::types::{ChannelSendStatus, NoRx, NoTx};

#[derive(Clone)]
pub struct Channel<Tx = NoTx, Rx = NoRx> {
    tx: Tx,
    rx: Rx,
    queue: &'static str,
    source: &'static str,
    live_id: Option<String>,
}

impl<T: Send + 'static> Channel<Tx<spsc::Array<T>>, Arc<Mutex<Option<AsyncRx<spsc::Array<T>>>>>> {
    #[allow(dead_code)]
    pub fn spsc_bounded(queue: &'static str, source: &'static str, size: usize) -> Self {
        let (tx, rx) = spsc::bounded_blocking_async(size);
        Self {
            tx,
            rx: Arc::new(Mutex::new(Some(rx))),
            queue,
            source,
            live_id: None,
        }
    }
}

impl<T: Send + 'static> Channel<MTx<mpsc::Array<T>>, Arc<Mutex<Option<AsyncRx<mpsc::Array<T>>>>>> {
    pub fn mpsc_bounded(queue: &'static str, source: &'static str, size: usize) -> Self {
        let (tx, rx) = mpsc::bounded_blocking_async(size);
        Self {
            tx,
            rx: Arc::new(Mutex::new(Some(rx))),
            queue,
            source,
            live_id: None,
        }
    }
}

impl<T: Send + 'static> Channel<MTx<mpmc::Array<T>>, MAsyncRx<mpmc::Array<T>>> {
    #[allow(dead_code)]
    pub fn mpmc_bounded(queue: &'static str, source: &'static str, size: usize) -> Self {
        let (tx, rx) = mpmc::bounded_blocking_async(size);
        Self {
            tx,
            rx,
            queue,
            source,
            live_id: None,
        }
    }
}

impl<T: Clone + Send + 'static> Channel<broadcast::Sender<T>, NoRx> {
    pub fn broadcast(queue: &'static str, source: &'static str, size: usize) -> Self {
        let (tx, _) = broadcast::channel(size);
        Self {
            tx,
            rx: NoRx,
            queue,
            source,
            live_id: None,
        }
    }
}

impl<Tx, Rx> Channel<Tx, Rx> {
    pub fn with_live_id(mut self, live_id: impl Into<String>) -> Self {
        self.live_id = Some(live_id.into());
        self
    }

    pub fn with_source(mut self, source: &'static str) -> Self {
        self.source = source;
        self
    }

    pub fn source(&self) -> &'static str {
        self.source
    }

    fn on_queue_full(&self) {
        match &self.live_id {
            Some(live_id) => warn!(
                queue = self.queue,
                source = self.source,
                live_id = %live_id,
                "Queue is full under bounded policy"
            ),
            None => warn!(
                queue = self.queue,
                source = self.source,
                "Queue is full under bounded policy"
            ),
        }
        metrics::get_metrics().record_queue_drop(self.queue, "full");
    }

    fn on_queue_disconnected(&self) {
        match &self.live_id {
            Some(live_id) => warn!(
                queue = self.queue,
                source = self.source,
                live_id = %live_id,
                "Queue is disconnected"
            ),
            None => warn!(
                queue = self.queue,
                source = self.source,
                "Queue is disconnected"
            ),
        }
        metrics::get_metrics().record_queue_drop(self.queue, "disconnected");
    }

    fn on_no_subscriber(&self) {
        match &self.live_id {
            Some(live_id) => warn!(
                queue = self.queue,
                source = self.source,
                live_id = %live_id,
                "Broadcast channel has no active subscribers"
            ),
            None => warn!(
                queue = self.queue,
                source = self.source,
                "Broadcast channel has no active subscribers"
            ),
        }
        metrics::get_metrics().record_queue_drop(self.queue, "no_subscriber");
    }

    #[allow(private_bounds)]
    pub fn send<T>(&self, item: T) -> ChannelSendStatus
    where
        Tx: ChannelTx<T>,
        T: Send + 'static,
    {
        match self.tx.channel_send(item) {
            Ok(()) => ChannelSendStatus::Sent,
            Err(SendError::Full(_)) => {
                self.on_queue_full();
                ChannelSendStatus::Full
            }
            Err(SendError::Disconnected(_)) => {
                self.on_queue_disconnected();
                ChannelSendStatus::Disconnected
            }
            Err(SendError::NoSubscriber(_)) => {
                self.on_no_subscriber();
                ChannelSendStatus::Disconnected
            }
        }
    }
}

impl<Tx, F: Flavor> Channel<Tx, Arc<Mutex<Option<AsyncRx<F>>>>> {
    pub fn subscribe(
        &self,
        listener_name: &'static str,
    ) -> Result<ChannelStream<AsyncRx<F>>, SubscribeError> {
        let mut guard = self.rx.lock().expect("receiver lock poisoned");
        let rx = guard
            .take()
            .ok_or_else(|| SubscribeError::already_subscribed(self.queue, self.source))?;

        Ok(ChannelStream::new(
            rx,
            self.queue,
            listener_name,
            self.live_id.clone(),
        ))
    }
}

impl<Tx, F: Flavor + FlavorMC> Channel<Tx, MAsyncRx<F>> {
    pub fn subscribe(&self, listener_name: &'static str) -> ChannelStream<MAsyncRx<F>> {
        ChannelStream::new(
            self.rx.clone(),
            self.queue,
            listener_name,
            self.live_id.clone(),
        )
    }
}

impl<T: Clone + Send + 'static> Channel<broadcast::Sender<T>, NoRx> {
    pub fn subscribe(&self, listener_name: &'static str) -> ChannelStream<broadcast::Receiver<T>> {
        ChannelStream::new(
            self.tx.subscribe(),
            self.queue,
            listener_name,
            self.live_id.clone(),
        )
    }
}
