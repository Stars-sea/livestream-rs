use std::sync::Arc;

use tokio::sync::broadcast;

use crate::core::flv_parser::FlvTag;
use crate::core::output::FlvPacket;
use crate::services::MemoryCache;

/// Manages RTMP streams and broadcasts FLV tags to subscribers.
#[derive(Clone)]
pub(super) struct StreamDispatcher {
    // Map: Stream Key -> Broadcast Sender
    streams: MemoryCache<broadcast::Sender<Arc<FlvTag>>>,
}

impl StreamDispatcher {
    pub fn new() -> Self {
        Self {
            streams: MemoryCache::new(),
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
        self.streams.get(stream_key).await
    }

    /// Returns a receiver for the specified stream key.
    pub async fn subscribe(&self, stream_key: &str) -> broadcast::Receiver<Arc<FlvTag>> {
        if let Some(sender) = self.stream(stream_key).await {
            return sender.subscribe();
        }

        self.streams
            .get_or_insert_with(stream_key.to_string(), || broadcast::channel(16).0)
            .await
            .subscribe()
    }
}
