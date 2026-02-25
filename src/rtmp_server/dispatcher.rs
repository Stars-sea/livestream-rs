use std::sync::Arc;

use tokio::sync::{RwLock, broadcast};

use crate::core::flv_parser::FlvTag;
use crate::services::MemoryCache;

#[derive(Clone)]
pub(super) struct StreamState {
    pub sender: broadcast::Sender<Arc<FlvTag>>,
    pub video_seq_header: Arc<RwLock<Option<Arc<FlvTag>>>>,
    pub audio_seq_header: Arc<RwLock<Option<Arc<FlvTag>>>>,
    pub metadata: Arc<RwLock<Option<Arc<FlvTag>>>>,
}

impl StreamState {
    pub fn new() -> Self {
        Self {
            sender: broadcast::channel(100).0,
            video_seq_header: Arc::new(RwLock::new(None)),
            audio_seq_header: Arc::new(RwLock::new(None)),
            metadata: Arc::new(RwLock::new(None)),
        }
    }
}

/// Manages RTMP streams and broadcasts FLV tags to subscribers.
#[derive(Clone)]
pub(super) struct StreamDispatcher {
    // Map: Stream Key -> StreamState
    pub streams: MemoryCache<StreamState>,
}

impl StreamDispatcher {
    pub fn new() -> Self {
        Self {
            streams: MemoryCache::new(),
        }
    }

    pub async fn stream(&self, stream_key: &str) -> Option<StreamState> {
        self.streams.get(stream_key).await
    }

    /// Returns a receiver for the specified stream key, along with cached headers.
    pub async fn subscribe(
        &self,
        stream_key: &str,
    ) -> (broadcast::Receiver<Arc<FlvTag>>, StreamState) {
        let state = self
            .streams
            .get_or_insert_with(stream_key.to_string(), || StreamState::new())
            .await;

        (state.sender.subscribe(), state)
    }
}
