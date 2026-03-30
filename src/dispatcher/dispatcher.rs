use std::sync::Arc;

use tokio::sync::{OnceCell, broadcast};

use super::SessionEvent;

static DISPATCHER: OnceCell<Arc<EventDispatcher>> = OnceCell::const_new();

pub async fn singleton() -> Arc<EventDispatcher> {
    DISPATCHER
        .get_or_init(async || Arc::new(EventDispatcher::new()))
        .await
        .clone()
}

pub struct EventDispatcher {
    sender: broadcast::Sender<SessionEvent>,
}

impl EventDispatcher {
    fn new() -> Self {
        let (sender, _) = broadcast::channel(16);
        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.sender.subscribe()
    }

    pub fn send(&self, event: SessionEvent) {
        let _ = self.sender.send(event);
    }
}
