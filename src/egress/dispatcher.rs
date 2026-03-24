use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use tokio::sync::RwLock;

use super::broadcaster::BroadcastRx;
use super::broadcaster::Broadcaster;
use crate::media::flv_parser::FlvTag;

/// Per-stream cache of headers required by late-joining RTMP players.
///
/// Responsibilities:
/// - Capture latest metadata/audio/video sequence headers.
/// - Provide replay set for egress bootstrap.
///
/// Out of scope:
/// - No subscriber lifecycle management.
pub struct StreamState {
    video_seq_header: Option<Arc<FlvTag>>,
    audio_seq_header: Option<Arc<FlvTag>>,
    metadata: Option<Arc<FlvTag>>,
}

impl StreamState {
    pub fn new() -> Self {
        Self {
            video_seq_header: None,
            audio_seq_header: None,
            metadata: None,
        }
    }

    pub fn update_from_tag(&mut self, tag: Arc<FlvTag>) {
        match tag.as_ref() {
            FlvTag::Video { payload, .. } => {
                if payload.len() > 1 && payload[0] == 0x17 && payload[1] == 0x00 {
                    self.video_seq_header = Some(tag);
                }
            }
            FlvTag::Audio { payload, .. } => {
                if payload.len() > 1 && payload[0] == 0xaf && payload[1] == 0x00 {
                    self.audio_seq_header = Some(tag);
                }
            }
            FlvTag::ScriptData { .. } => {
                self.metadata = Some(tag);
            }
        }
    }

    pub fn cached_headers(&self) -> Vec<Arc<FlvTag>> {
        let mut tags = Vec::with_capacity(3);

        if let Some(tag) = self.metadata.clone() {
            tags.push(tag);
        }
        if let Some(tag) = self.video_seq_header.clone() {
            tags.push(tag);
        }
        if let Some(tag) = self.audio_seq_header.clone() {
            tags.push(tag);
        }

        tags
    }
}

/// Fan-out dispatcher from parsed FLV tags to stream subscribers.
///
/// Responsibilities:
/// - Maintain stream-keyed broadcaster channels.
/// - Keep and expose stream state for cached header replay.
///
/// Out of scope:
/// - No RTMP protocol encoding.
#[derive(Clone)]
pub struct StreamDispatcher {
    broadcaster: Broadcaster<String, Arc<FlvTag>>,
    streams: DashMap<String, Arc<RwLock<StreamState>>>,
}

impl StreamDispatcher {
    pub fn new() -> Self {
        Self {
            broadcaster: Broadcaster::new(),
            streams: DashMap::new(),
        }
    }

    pub fn stream(&self, stream_key: &str) -> Arc<RwLock<StreamState>> {
        self.streams
            .entry(stream_key.to_string())
            .or_insert_with(|| Arc::new(RwLock::new(StreamState::new())))
            .clone()
    }

    pub fn remove_stream(&self, stream_key: &str) {
        self.streams.remove(stream_key);
    }

    pub fn subscribe(
        &self,
        stream_key: &str,
    ) -> (BroadcastRx<Arc<FlvTag>>, Arc<RwLock<StreamState>>) {
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
        state
            .write()
            .await
            .update_from_tag(Arc::new(FlvTag::ScriptData {
                timestamp: 3,
                payload: bytes::Bytes::from_static(&[0x12]),
            }));

        dispatcher.remove_stream("live_2");

        let recreated = dispatcher.stream("live_2");
        let recreated = recreated.read().await;
        assert!(recreated.metadata.is_none());
        assert!(recreated.audio_seq_header.is_none());
        assert!(recreated.video_seq_header.is_none());
    }
}
