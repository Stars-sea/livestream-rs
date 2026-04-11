use tokio::sync::{RwLock, broadcast};

use crate::infra::media::packet::FlvTag;
use crate::queue::{BroadcastChannel, Channel, ChannelSendStatus, ChannelStream};

/// Holds the caching state (Sequence Headers & ScriptData) of a live stream.
#[derive(Default)]
struct StreamCache {
    video_seq: Option<FlvTag>,
    audio_seq: Option<FlvTag>,
    script_data: Option<FlvTag>,
}

/// A thread-safe broadcaster for a single RTMP live stream channel.
/// Manages late-joiner protocol requirements (e.g. Decoder Configuration/Sequence Headers).
pub struct LiveChannel {
    sender: BroadcastChannel<FlvTag>,
    cache: RwLock<StreamCache>,
}

impl LiveChannel {
    pub fn new() -> Self {
        Self {
            sender: Channel::broadcast("rtmp_live_tag", "transport.rtmp.live_channel", 1024),
            cache: RwLock::new(StreamCache::default()),
        }
    }

    /// Broadcasts an incoming FLV tag to all subscribers and updates cache.
    pub async fn broadcast_tag(
        &self,
        tag: FlvTag,
    ) -> Result<usize, broadcast::error::SendError<FlvTag>> {
        // Cache only initialization tags for late subscribers.
        if tag.is_sequence_header() {
            let mut cache = self.cache.write().await;
            match &tag {
                FlvTag::ScriptData(_) => cache.script_data = Some(tag.clone()),
                FlvTag::Video { .. } => cache.video_seq = Some(tag.clone()),
                FlvTag::Audio { .. } => cache.audio_seq = Some(tag.clone()),
            }
        }

        match self.sender.send(tag.clone()) {
            ChannelSendStatus::Sent => Ok(1),
            ChannelSendStatus::Disconnected => Err(broadcast::error::SendError(tag)),
            ChannelSendStatus::Full => Ok(0),
        }
    }

    /// Subscribes to the channel and returns the real-time receiver along with
    /// any cached initialization tags that missed the live broadcast.
    pub async fn subscribe(
        &self,
        listener_name: &'static str,
    ) -> (ChannelStream<broadcast::Receiver<FlvTag>>, Vec<FlvTag>) {
        let rx = self.sender.subscribe(listener_name);

        let mut initial_tags = Vec::with_capacity(3);
        let cache = self.cache.read().await;
        if let Some(t) = &cache.script_data {
            initial_tags.push(t.clone());
        }
        if let Some(t) = &cache.video_seq {
            initial_tags.push(t.clone());
        }
        if let Some(t) = &cache.audio_seq {
            initial_tags.push(t.clone());
        }

        (rx, initial_tags)
    }
}
