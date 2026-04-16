use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;

use crate::infra::media::stream::StreamCollection;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Protocol {
    Rtmp,
    Srt,
}

#[derive(Clone)]
pub enum SessionEvent {
    SessionStarted {
        live_id: String,
        protocol: Protocol,
    },

    SessionInit {
        live_id: String,
        streams: Arc<dyn StreamCollection + Send + Sync>,
    },

    // TODO: add more fields, such as error code, error message, etc.
    SessionEnded {
        live_id: String,
        protocol: Protocol,
    },

    SegmentComplete {
        live_id: String,
        path: PathBuf,
    },
}

impl SessionEvent {
    pub fn id(&self) -> &str {
        match self {
            SessionEvent::SessionStarted { live_id, .. } => live_id,
            SessionEvent::SessionInit { live_id, .. } => live_id,
            SessionEvent::SessionEnded { live_id, .. } => live_id,
            SessionEvent::SegmentComplete { live_id, .. } => live_id,
        }
    }
}

impl Debug for SessionEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionEvent::SessionStarted { live_id, protocol } => f
                .debug_struct("SessionStarted")
                .field("live_id", live_id)
                .field("protocal", protocol)
                .finish(),
            SessionEvent::SessionInit { live_id, .. } => f
                .debug_struct("StreamInitialized")
                .field("live_id", live_id)
                .field("streams", &"<stream collection>")
                .finish(),
            SessionEvent::SessionEnded {
                live_id,
                protocol: protocal,
            } => f
                .debug_struct("SessionEnded")
                .field("live_id", live_id)
                .field("protocal", protocal)
                .finish(),
            SessionEvent::SegmentComplete { live_id, path } => f
                .debug_struct("SegmentComplete")
                .field("live_id", live_id)
                .field("path", path)
                .finish(),
        }
    }
}
