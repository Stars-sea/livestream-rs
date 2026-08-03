//! FlvEgressHub — central FLV tag distribution hub.
//!
//! Implements `FlvBroadcast` trait from `livestream-pipeline`, bridging
//! the pipeline's `FlvSink` to all RTMP/HTTP-FLV subscribers.

use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use livestream_core::pad::DemandSignal;
use livestream_media::flv::FlvTag;
use livestream_pipeline::broadcast::FlvBroadcast;
use tokio::sync::broadcast;

use livestream_telemetry::metric_queue_drop;

use super::channel::FlvLiveChannel;

/// Central hub for FLV tag distribution.  Maps stream IDs to channels.
pub struct FlvEgressHub {
    channels: DashMap<String, Arc<FlvLiveChannel>>,
    demand_signals: DashMap<String, DemandSignal>,
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
            demand_signals: DashMap::new(),
        }
    }

    /// Get or create a channel for the given stream.
    /// Also ensures a demand signal entry exists for the stream.
    pub fn create_channel(&self, live_id: &str) -> Arc<FlvLiveChannel> {
        self.demand_signals.entry(live_id.to_string()).or_default();
        self.channels
            .entry(live_id.to_string())
            .or_insert_with(|| Arc::new(FlvLiveChannel::new()))
            .value()
            .clone()
    }

    /// Remove a stream's channel (on stream end).
    /// Also removes the demand signal so both maps stop growing for
    /// stopped/expired streams.
    pub fn remove_channel(&self, live_id: &str) {
        self.channels.remove(live_id);
        self.demand_signals.remove(live_id);
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
        match self.channels.get(live_id) {
            Some(ch) => {
                if let Err(e) = ch.broadcast(&tag) {
                    metric_queue_drop!("flv_broadcast", "no_receivers");
                    tracing::debug!(live_id = %live_id, error = %e, "FlvEgressHub: no active subscribers");
                }
            }
            None => {
                tracing::debug!(live_id = %live_id, "FlvEgressHub: channel not found");
            }
        }
        Ok(())
    }

    fn subscribe(&self, live_id: &str) -> livestream_core::pad::DemandHandle {
        self.demand_signals
            .entry(live_id.to_string())
            .or_default()
            .new_handle()
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use livestream_core::pad::DemandHandle;

    fn sample_tag() -> FlvTag {
        FlvTag::video(0, Bytes::from_static(&[0x17, 0x00, 0, 0, 0, 0x01]))
    }

    #[test]
    fn create_channel_creates_and_reuses() {
        let hub = FlvEgressHub::new();
        let a = hub.create_channel("stream-a");
        let a_again = hub.create_channel("stream-a");
        let b = hub.create_channel("stream-b");
        assert!(Arc::ptr_eq(&a, &a_again), "same stream reuses the channel");
        assert!(
            !Arc::ptr_eq(&a, &b),
            "different streams get distinct channels"
        );
    }

    #[test]
    fn subscribe_returns_channel_only_after_create() {
        let hub = FlvEgressHub::new();
        assert!(hub.subscribe("missing").is_none());
        hub.create_channel("present");
        let (rx, _cached) = hub
            .subscribe("present")
            .expect("created stream should be subscribable");
        drop(rx);
    }

    #[test]
    fn remove_channel_cleans_up() {
        let hub = FlvEgressHub::new();
        hub.create_channel("ephemeral");
        assert!(hub.subscribe("ephemeral").is_some());
        hub.remove_channel("ephemeral");
        assert!(hub.subscribe("ephemeral").is_none());
        // 重新 create 得到新 channel
        let fresh = hub.create_channel("ephemeral");
        assert!(
            fresh.subscribe().0.try_recv().is_err(),
            "fresh channel has no cached tags"
        );
    }

    #[tokio::test]
    async fn broadcast_delivers_to_subscriber() {
        let hub = FlvEgressHub::new();
        hub.create_channel("stream-a");
        let (mut rx, _cached) = hub.subscribe("stream-a").unwrap();
        let tag = sample_tag();
        hub.broadcast("stream-a", tag.clone()).await.unwrap();
        let received = rx.recv().await.expect("subscriber should receive the tag");
        assert_eq!(received.payload_size(), tag.payload_size());
    }

    #[tokio::test]
    async fn broadcast_without_channel_is_ok() {
        let hub = FlvEgressHub::new();
        let result = hub.broadcast("nowhere", sample_tag()).await;
        assert!(result.is_ok(), "missing channel is not an error");
    }

    #[tokio::test]
    async fn broadcast_without_receivers_is_ok() {
        let hub = FlvEgressHub::new();
        hub.create_channel("silent");
        let result = hub.broadcast("silent", sample_tag()).await;
        assert!(result.is_ok(), "no subscribers is not an error");
    }

    #[test]
    fn demand_handle_subscribe() {
        let hub = FlvEgressHub::new();
        let handle: DemandHandle = FlvBroadcast::subscribe(&hub, "stream-a");
        // 句柄可 clone/drop，不依赖 channel 存在
        let cloned = handle.clone();
        drop(cloned);
        drop(handle);
    }
}
