use std::sync::Mutex;

use livestream_core::pad::DemandHandle;
use livestream_media::flv::FlvTag;
use livestream_pipeline::broadcast::FlvBroadcast;

/// Spy implementation of FlvBroadcast that records all broadcasted tags.
pub struct SpyFlvBroadcast {
    tags: Mutex<Vec<FlvTag>>,
}

impl SpyFlvBroadcast {
    pub fn new() -> Self {
        Self {
            tags: Mutex::new(Vec::new()),
        }
    }

    pub fn tags(&self) -> Vec<FlvTag> {
        self.tags.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl FlvBroadcast for SpyFlvBroadcast {
    async fn broadcast(&self, _live_id: &str, tag: FlvTag) -> anyhow::Result<()> {
        self.tags.lock().unwrap().push(tag);
        Ok(())
    }

    fn subscribe(&self, _live_id: &str) -> DemandHandle {
        livestream_core::pad::DemandSignal::new_always_wanted().new_handle()
    }
}
