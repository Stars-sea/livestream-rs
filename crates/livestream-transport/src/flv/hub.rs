//! FlvEgressHub — central FLV tag distribution hub.
//!
//! Implements `FlvBroadcast` trait from `livestream-pipeline`, bridging
//! the pipeline's `FlvSink` to all RTMP/HTTP-FLV subscribers.

use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use livestream_media::flv::FlvTag;
use livestream_pipeline::broadcast::FlvBroadcast;
use tokio::sync::broadcast;

use super::channel::FlvLiveChannel;

/// Central hub for FLV tag distribution.  Maps stream IDs to channels.
pub struct FlvEgressHub {
    channels: DashMap<String, Arc<FlvLiveChannel>>,
}

impl Default for FlvEgressHub {
    fn default() -> Self {
        Self::new()
    }
}

impl FlvEgressHub {
    pub fn new() -> Self {
        Self {
            channels: DashMap::new(),
        }
    }

    /// Get or create a channel for the given stream.
    pub fn create_channel(&self, live_id: &str) -> Arc<FlvLiveChannel> {
        self.channels
            .entry(live_id.to_string())
            .or_insert_with(|| Arc::new(FlvLiveChannel::new()))
            .value()
            .clone()
    }

    /// Remove a stream's channel (on stream end).
    pub fn remove_channel(&self, live_id: &str) {
        self.channels.remove(live_id);
    }

    /// Subscribe to a stream. Returns the broadcast receiver and cached
    /// initialization tags, or None if the stream doesn't exist.
    pub fn subscribe(&self, live_id: &str) -> Option<(broadcast::Receiver<FlvTag>, Vec<FlvTag>)> {
        self.channels.get(live_id).map(|ch| ch.subscribe())
    }
}

/// Implementation of `FlvBroadcast` trait.
///
/// This is the key bridge: `FlvSink` in `livestream-pipeline` holds an
/// `Arc<dyn FlvBroadcast>` and calls `broadcast()`.  `FlvEgressHub`
/// implements that trait, distributing tags to all subscribers.
#[async_trait::async_trait]
impl FlvBroadcast for FlvEgressHub {
    async fn broadcast(&self, live_id: &str, tag: FlvTag) -> Result<()> {
        if let Some(ch) = self.channels.get(live_id) {
            let _ = ch.broadcast(&tag);
        }
        Ok(())
    }
}
