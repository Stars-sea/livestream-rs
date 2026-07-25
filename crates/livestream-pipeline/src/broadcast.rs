//! FlvBroadcast trait — breaks circular dependency between pipeline and transport.
//!
//! Defined in `livestream-pipeline` and implemented in `livestream-transport`
//! by `FlvEgressHub` (Phase 5).

use anyhow::Result;
use livestream_media::flv::FlvTag;

/// Broadcast an FLV tag to all subscribers of a live stream.
///
/// `FlvSink` depends on `Arc<dyn FlvBroadcast>` rather than on the concrete
/// `FlvEgressHub` type. The actual implementation lives in `livestream-transport`
/// and is injected at pipeline construction time.
#[async_trait::async_trait]
pub trait FlvBroadcast: Send + Sync {
    /// Send an FLV tag to all RTMP/HTTP-FLV subscribers for the given stream.
    async fn broadcast(&self, live_id: &str, tag: FlvTag) -> Result<()>;
}
