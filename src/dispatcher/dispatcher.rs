use std::sync::{Arc, LazyLock};

use dashmap::{DashMap, Entry};

use super::SessionEvent;
use crate::channel::{self, BroadcastRx, BroadcastTx};

pub static INSTANCE: LazyLock<Arc<EventDispatcher>> =
    LazyLock::new(|| Arc::new(EventDispatcher::new()));

pub struct EventDispatcher {
    channel: BroadcastTx<SessionEvent>,

    senders: DashMap<String, BroadcastTx<SessionEvent>>,
}

impl EventDispatcher {
    fn new() -> Self {
        // TODO: configure channel capacity in config.rs
        let (tx, _) = channel::broadcast("session_event", None, 16);
        Self {
            channel: tx,
            senders: DashMap::new(),
        }
    }

    pub fn subscribe_global(&self) -> BroadcastRx<SessionEvent> {
        self.channel.subscribe()
    }

    pub fn subscribe(&self, live_id: impl Into<String>) -> BroadcastRx<SessionEvent> {
        let live_id = live_id.into();

        let entry = self.senders.entry(live_id.clone()).or_insert_with(|| {
            let (tx, _) = channel::broadcast("sub_session_event", Some(live_id.clone()), 16);
            tx
        });

        entry.value().subscribe()
    }

    pub fn send(&self, event: SessionEvent) {
        if let Entry::Occupied(entry) = self.senders.entry(event.id().to_string()) {
            if !entry.get().send(event.clone()).is_ok_and(|n| n != 0) {
                // If send returns an error, it means there are no active subscribers.
                // We can remove the sender to free up resources.
                entry.remove();
            } else if matches!(event, SessionEvent::SessionEnded { .. }) {
                // If the event is SessionEnded, we can also remove the sender to clean up.
                entry.remove();
            }
        }

        let _ = self.channel.send(event);
    }
}
