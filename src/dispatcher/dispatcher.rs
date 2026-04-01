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

    #[allow(unused)]
    pub fn on_session_event<Fut, F>(&self, callback: F)
    where
        F: Fn(SessionEvent) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut rx = self.subscribe();
        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                callback(event).await;
            }
        });
    }

    pub fn send(&self, event: SessionEvent) {
        let _ = self.sender.send(event);
    }
}
