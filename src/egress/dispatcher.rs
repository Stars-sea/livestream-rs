use std::sync::Arc;

use tokio::sync::{RwLock, broadcast};

use crate::media::flv_parser::FlvTag;
use crate::infra::MemoryCache;

#[derive(Clone)]
pub(crate) struct StreamState {
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

#[derive(Clone)]
pub(crate) struct StreamDispatcher {
    streams: MemoryCache<StreamState>,
}

impl StreamDispatcher {
    pub fn new() -> Self {
        Self {
            streams: MemoryCache::new(),
        }
    }

    pub async fn stream(&self, stream_key: &str) -> StreamState {
        self.streams
            .get_or_insert_with(stream_key.to_string(), StreamState::new)
            .await
    }

    pub async fn remove_stream(&self, stream_key: &str) {
        self.streams.remove(stream_key).await;
    }

    pub async fn subscribe(
        &self,
        stream_key: &str,
    ) -> (broadcast::Receiver<Arc<FlvTag>>, StreamState) {
        let state = self.stream(stream_key).await;
        (state.sender.subscribe(), state)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::media::flv_parser::FlvTag;

    use super::StreamDispatcher;

    #[tokio::test]
    async fn subscribe_receives_broadcast_tag() {
        let dispatcher = StreamDispatcher::new();
        let (mut rx, state) = dispatcher.subscribe("live_1").await;

        let tag = Arc::new(FlvTag::ScriptData {
            timestamp: 1,
            payload: bytes::Bytes::from_static(&[0x02, 0x00]),
        });

        let sent = state.sender.send(tag.clone());
        assert!(sent.is_ok());

        let received = rx.recv().await.expect("receiver should get tag");
        match received.as_ref() {
            FlvTag::ScriptData { timestamp, .. } => assert_eq!(*timestamp, 1),
            _ => panic!("expected script data tag"),
        }
    }

    #[tokio::test]
    async fn remove_stream_resets_state_on_recreate() {
        let dispatcher = StreamDispatcher::new();

        let state = dispatcher.stream("live_2").await;
        *state.metadata.write().await = Some(Arc::new(FlvTag::ScriptData {
            timestamp: 3,
            payload: bytes::Bytes::from_static(&[0x12]),
        }));

        dispatcher.remove_stream("live_2").await;

        let recreated = dispatcher.stream("live_2").await;
        assert!(recreated.metadata.read().await.is_none());
        assert!(recreated.audio_seq_header.read().await.is_none());
        assert!(recreated.video_seq_header.read().await.is_none());
    }
}
