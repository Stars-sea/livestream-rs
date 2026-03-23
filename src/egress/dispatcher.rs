use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use tokio::sync::RwLock;

use super::broadcaster::BroadcastRx;
use super::broadcaster::Broadcaster;
use crate::media::flv_parser::FlvTag;

pub(crate) struct StreamState {
    pub video_seq_header: RwLock<Option<Arc<FlvTag>>>,
    pub audio_seq_header: RwLock<Option<Arc<FlvTag>>>,
    pub metadata: RwLock<Option<Arc<FlvTag>>>,
}

impl StreamState {
    pub fn new() -> Self {
        Self {
            video_seq_header: RwLock::new(None),
            audio_seq_header: RwLock::new(None),
            metadata: RwLock::new(None),
        }
    }
}

#[derive(Clone)]
pub(crate) struct StreamDispatcher {
    broadcaster: Broadcaster<String, Arc<FlvTag>>,
    streams: DashMap<String, Arc<StreamState>>,
}

impl StreamDispatcher {
    pub fn new() -> Self {
        Self {
            broadcaster: Broadcaster::new(),
            streams: DashMap::new(),
        }
    }

    pub fn stream(&self, stream_key: &str) -> Arc<StreamState> {
        self.streams
            .entry(stream_key.to_string())
            .or_insert_with(|| Arc::new(StreamState::new()))
            .clone()
    }

    pub fn remove_stream(&self, stream_key: &str) {
        self.streams.remove(stream_key);
    }

    pub fn subscribe(&self, stream_key: &str) -> (BroadcastRx<Arc<FlvTag>>, Arc<StreamState>) {
        let state = self.stream(stream_key);
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
        let (mut rx, _state) = dispatcher.subscribe("live_1");

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

        let state = dispatcher.stream("live_2");
        *state.metadata.write().await = Some(Arc::new(FlvTag::ScriptData {
            timestamp: 3,
            payload: bytes::Bytes::from_static(&[0x12]),
        }));

        dispatcher.remove_stream("live_2");

        let recreated = dispatcher.stream("live_2");
        assert!(recreated.metadata.read().await.is_none());
        assert!(recreated.audio_seq_header.read().await.is_none());
        assert!(recreated.video_seq_header.read().await.is_none());
    }
}
