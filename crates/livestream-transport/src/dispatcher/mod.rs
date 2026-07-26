mod event;

use dashmap::{DashMap, Entry};
pub use event::{EndReason, SessionEvent};

use livestream_core::channel::{self, BroadcastRx, BroadcastTx};

pub struct EventDispatcher {
    channel: BroadcastTx<SessionEvent>,
    senders: DashMap<String, BroadcastTx<SessionEvent>>,
}

impl EventDispatcher {
    pub fn new() -> Self {
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
            let (tx, _) = channel::broadcast("sub_session_event", Some(&live_id), 16);
            tx
        });

        entry.value().subscribe()
    }

    pub fn send(&self, event: SessionEvent) {
        if let Entry::Occupied(entry) = self.senders.entry(event.id().to_string()) {
            let should_remove = !entry.get().send(event.clone()).is_ok_and(|n| n != 0)
                || matches!(event, SessionEvent::SessionEnded { .. });
            if should_remove {
                entry.remove();
            }
        }

        let _ = self.channel.send(event);
    }
}

impl Default for EventDispatcher {
    fn default() -> Self {
        Self::new()
    }
}
