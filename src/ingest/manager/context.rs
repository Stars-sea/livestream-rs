use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crossfire::{MTx, mpsc};

use super::state::ManagerStreamState;
use crate::ingest::StreamInfo;
use crate::ingest::adapters::RtmpTag;

/// Context for an active stream, holding its configuration, life-cycle controls,
/// and protocol-specific communication channels.
#[derive(Debug)]
pub struct StreamContext {
    /// Information and configuration (e.g., live ID, ports) of the current stream.
    pub info: Arc<StreamInfo>,
    /// Signal used to indicate that the stream should be stopped.
    pub stop_signal: Arc<AtomicBool>,
    /// Optional transmitter for routing RTMP media tags, if the stream uses the RTMP protocol.
    pub rtmp_tx: Option<MTx<mpsc::List<RtmpTag>>>,
    /// Stream lifecycle state managed by ManagerActor.
    pub state: ManagerStreamState,
}
