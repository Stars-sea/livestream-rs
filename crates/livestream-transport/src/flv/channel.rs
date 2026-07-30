//! FlvLiveChannel — per-stream broadcast channel for FLV tags.

use parking_lot::Mutex;

use livestream_media::flv::FlvTag;
use tokio::sync::broadcast;

/// A per-stream channel that broadcasts FLV tags to all subscribers.
///
/// Caches sequence headers (video/audio/metadata) so late-joining
/// subscribers receive the initialization data they need.
pub struct FlvLiveChannel {
    sender: broadcast::Sender<FlvTag>,
    video_seq: Mutex<Option<FlvTag>>,
    audio_seq: Mutex<Option<FlvTag>>,
    metadata: Mutex<Option<FlvTag>>,
}

impl Default for FlvLiveChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl FlvLiveChannel {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(1024);
        Self {
            sender,
            video_seq: Mutex::new(None),
            audio_seq: Mutex::new(None),
            metadata: Mutex::new(None),
        }
    }

    /// Broadcast a tag to all subscribers. Caches sequence headers for late joiners.
    pub fn broadcast(&self, tag: &FlvTag) -> Result<usize, broadcast::error::SendError<FlvTag>> {
        self.cache_seq_header(tag);
        self.sender.send(tag.clone())
    }

    fn cache_seq_header(&self, tag: &FlvTag) {
        if !tag.is_sequence_header() {
            return;
        }
        let slot = match tag {
            FlvTag::Video { .. } => &self.video_seq,
            FlvTag::Audio { .. } => &self.audio_seq,
            FlvTag::ScriptData(_) => &self.metadata,
        };
        *slot.lock() = Some(tag.clone());
    }
    pub fn subscribe(&self) -> (broadcast::Receiver<FlvTag>, Vec<FlvTag>) {
        let rx = self.sender.subscribe();
        let mut cached = Vec::new();
        if let Some(tag) = self.metadata.lock().clone() {
            cached.push(tag);
        }
        if let Some(tag) = self.video_seq.lock().clone() {
            cached.push(tag);
        }
        if let Some(tag) = self.audio_seq.lock().clone() {
            cached.push(tag);
        }
        (rx, cached)
    }
}
