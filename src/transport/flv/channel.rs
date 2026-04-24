use tokio::sync::RwLock;

use crate::channel::{self, BroadcastRx, BroadcastTx, SendError};
use crate::infra::media::packet::FlvTag;

#[derive(Default)]
struct StreamCache {
    video_seq: Option<FlvTag>,
    audio_seq: Option<FlvTag>,
    script_data: Option<FlvTag>,
}

pub struct LiveChannel {
    sender: BroadcastTx<FlvTag>,
    cache: RwLock<StreamCache>,
}

impl LiveChannel {
    pub fn new(live_id: impl Into<String>) -> Self {
        let (tx, _) = channel::broadcast("flv-live-channel", Some(live_id.into()), 1024);
        Self {
            sender: tx,
            cache: RwLock::new(StreamCache::default()),
        }
    }

    pub async fn broadcast_tag(&self, tag: FlvTag) -> Result<(), SendError> {
        if tag.is_sequence_header() {
            let mut cache = self.cache.write().await;
            match &tag {
                FlvTag::ScriptData(_) => cache.script_data = Some(tag.clone()),
                FlvTag::Video { .. } => cache.video_seq = Some(tag.clone()),
                FlvTag::Audio { .. } => cache.audio_seq = Some(tag.clone()),
            }
        }

        self.sender.send(tag)?;
        Ok(())
    }

    pub async fn subscribe(&self) -> (BroadcastRx<FlvTag>, Vec<FlvTag>) {
        let rx = self.sender.subscribe();

        let mut initial_tags = Vec::with_capacity(3);
        let cache = self.cache.read().await;
        if let Some(tag) = &cache.script_data {
            initial_tags.push(tag.clone());
        }
        if let Some(tag) = &cache.video_seq {
            initial_tags.push(tag.clone());
        }
        if let Some(tag) = &cache.audio_seq {
            initial_tags.push(tag.clone());
        }

        (rx, initial_tags)
    }
}
