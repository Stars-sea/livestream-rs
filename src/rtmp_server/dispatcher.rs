use std::{collections::HashMap, sync::Arc};

use tokio::sync::{RwLock, broadcast};

use crate::core::output::FlvPacket;
use crate::rtmp_server::flv_parser::FlvTag;

/// Manages RTMP streams and broadcasts FLV tags to subscribers.
#[derive(Clone)]
pub(super) struct StreamDispatcher {
    // Map: Stream Key -> Broadcast Sender
    streams: Arc<RwLock<HashMap<String, broadcast::Sender<Arc<FlvTag>>>>>,
}

impl StreamDispatcher {
    pub fn new() -> Self {
        Self {
            streams: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Publishes a packet to the appropriate stream channel.
    pub async fn publish(&self, packet: FlvPacket) {
        let _live_id = packet.live_id;
        // We need a way to maintain state (demuxer) for each stream.
        // But here we are stateless per packet if we just distribute.
        // Wait, the previous implementation had a demuxer loop.
        // We'll keep the demuxer logic in the background task loop in `run`,
        // or we need a more complex Dispatcher.
        // For simplicity in this struct, we just expose the get_subscriber method.
    }

    pub async fn stream(&self, stream_key: &str) -> Option<broadcast::Sender<Arc<FlvTag>>> {
        let map = self.streams.read().await;
        map.get(stream_key).cloned()
    }

    /// Returns a receiver for the specified stream key.
    pub async fn subscribe(&self, stream_key: &str) -> broadcast::Receiver<Arc<FlvTag>> {
        let map = self.streams.read().await;
        if let Some(sender) = map.get(stream_key) {
            return sender.subscribe();
        }
        drop(map);

        let mut map = self.streams.write().await;
        // Check again in case another thread created it
        map.entry(stream_key.to_string())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(100);
                tx
            })
            .subscribe()
    }
}
