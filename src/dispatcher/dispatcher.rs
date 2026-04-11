use std::sync::Arc;

use tokio::sync::OnceCell;

use super::{SessionEvent, SessionEventStream};
use crate::queue::{BroadcastChannel, Channel};

static DISPATCHER: OnceCell<Arc<EventDispatcher>> = OnceCell::const_new();

pub async fn singleton() -> Arc<EventDispatcher> {
    DISPATCHER
        .get_or_init(async || Arc::new(EventDispatcher::new()))
        .await
        .clone()
}

pub struct EventDispatcher {
    channel: BroadcastChannel<SessionEvent>,
}

impl EventDispatcher {
    fn new() -> Self {
        Self {
            channel: Channel::broadcast("session_event", "dispatcher.event", 16),
        }
    }

    pub fn subscribe_stream(&self, listener_name: &'static str) -> SessionEventStream {
        self.channel.subscribe(listener_name)
    }

    #[allow(unused)]
    pub fn on_session_event<Fut, F>(&self, callback: F)
    where
        F: Fn(SessionEvent) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut events = self.subscribe_stream("dispatcher.on_session_event");
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
