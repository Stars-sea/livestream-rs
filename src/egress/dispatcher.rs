use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::egress::broadcaster::BroadcastRx;
use crate::egress::broadcaster::Broadcaster;
use crate::infra::MemoryCache;
use crate::media::flv_parser::FlvTag;

#[derive(Clone)]
pub(crate) struct StreamState {
    pub video_seq_header: Arc<RwLock<Option<Arc<FlvTag>>>>,
    pub audio_seq_header: Arc<RwLock<Option<Arc<FlvTag>>>>,
    pub metadata: Arc<RwLock<Option<Arc<FlvTag>>>>,
}

impl StreamState {
    pub fn new() -> Self {
        Self {
            video_seq_header: Arc::new(RwLock::new(None)),
            audio_seq_header: Arc::new(RwLock::new(None)),
            metadata: Arc::new(RwLock::new(None)),
        }
    }
}

#[derive(Clone)]
pub(crate) struct StreamDispatcher {
    broadcaster: Broadcaster<String, Arc<FlvTag>>,
    streams: MemoryCache<StreamState>,
}

impl StreamDispatcher {
    pub fn new() -> Self {
        Self {
            broadcaster: Broadcaster::new(),
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

    pub async fn subscribe(&self, stream_key: &str) -> (BroadcastRx<Arc<FlvTag>>, StreamState) {
        let state = self.stream(stream_key).await;
        (self.broadcaster.subscribe(stream_key.to_string()), state)
    }

    pub fn send(&self, stream_key: &str, tag: Arc<FlvTag>) -> Result<usize> {
        self.broadcaster.send(stream_key.to_string(), tag)
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
        let (mut rx, _state) = dispatcher.subscribe("live_1").await;

        let tag = Arc::new(FlvTag::ScriptData {
            timestamp: 1,
            payload: bytes::Bytes::from_static(&[0x02, 0x00]),
        });

        let sent = dispatcher.send("live_1", tag.clone());
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
