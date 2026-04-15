use std::sync::{Arc, LazyLock};

use super::SessionEvent;
use crate::channel::{self, BroadcastRx, BroadcastTx};

pub static INSTANCE: LazyLock<Arc<EventDispatcher>> =
    LazyLock::new(|| Arc::new(EventDispatcher::new()));

pub struct EventDispatcher {
    channel: BroadcastTx<SessionEvent>,
}

impl EventDispatcher {
    fn new() -> Self {
        let (tx, _) = channel::broadcast("session_event", None, 16);
        Self { channel: tx }
    }

    pub fn subscribe_stream(&self) -> BroadcastRx<SessionEvent> {
        self.channel.subscribe()
    }

    #[allow(unused)]
    pub fn on_session_event<Fut, F>(&self, callback: F)
    where
        F: Fn(SessionEvent) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut events = self.subscribe_stream();
        tokio::spawn(async move {
            while let Some(event) = events.next().await {
                callback(event).await;
            }
        });
    }

    pub fn send(&self, event: SessionEvent) {
        let _ = self.channel.send(event);
    }
}
