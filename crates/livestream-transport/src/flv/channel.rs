//! FlvLiveChannel — per-stream broadcast channel for FLV tags.

use std::sync::Mutex;

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
        if tag.is_sequence_header() {
            match tag {
                FlvTag::Video { .. } => {
                    *self.video_seq.lock().unwrap() = Some(tag.clone());
                }
                FlvTag::Audio { .. } => {
                    *self.audio_seq.lock().unwrap() = Some(tag.clone());
                }
                FlvTag::ScriptData(_) => {
                    *self.metadata.lock().unwrap() = Some(tag.clone());
                }
            }
        }
        self.sender.send(tag.clone())
    }

    /// Subscribe to this channel. Returns the receiver and any cached
    /// initialization tags (video seq, audio seq, metadata).
    pub fn subscribe(&self) -> (broadcast::Receiver<FlvTag>, Vec<FlvTag>) {
        let rx = self.sender.subscribe();
        let mut cached = Vec::new();
        if let Some(tag) = self.metadata.lock().unwrap().clone() {
            cached.push(tag);
        }
        if let Some(tag) = self.video_seq.lock().unwrap().clone() {
            cached.push(tag);
        }
        if let Some(tag) = self.audio_seq.lock().unwrap().clone() {
            cached.push(tag);
        }
        (rx, cached)
    }
}
