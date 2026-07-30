//! Shared configuration structs for transport servers.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::controller::ControlMessage;
use crate::dispatcher::EventDispatcher;
use crate::flv::FlvEgressHub;
use crate::registry::SessionRegistry;
use livestream_codec::SegmentConfig;
use livestream_core::channel::MpscRx;
use livestream_pipeline::sink::minio::ObjectUploader;
use tokio_util::sync::CancellationToken;

/// Configuration shared by RTMP and RTSP ingest servers.
pub struct ServerConfig {
    pub addr: SocketAddr,
    pub ctrl_channel: MpscRx<ControlMessage>,
    pub flv_egress_hub: Arc<FlvEgressHub>,
    pub registry: Arc<SessionRegistry>,
    pub dispatcher: Arc<EventDispatcher>,
    pub precreate_ttl: Duration,
    pub minio: Arc<dyn ObjectUploader>,
    pub segment_cfg: SegmentConfig,
    pub cancel_token: CancellationToken,
    /// Maximum concurrent connections (0 = unlimited).
    pub max_connections: usize,
}

/// Build an optional connection-limit Semaphore from a usize limit.
/// Returns `None` when `limit` is 0 (unlimited).
pub(crate) fn make_connection_semaphore(limit: usize) -> Option<Arc<tokio::sync::Semaphore>> {
    if limit > 0 {
        Some(Arc::new(tokio::sync::Semaphore::new(limit)))
    } else {
        None
    }
}
