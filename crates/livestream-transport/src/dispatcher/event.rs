use std::fmt::Debug;
use std::sync::Arc;

use livestream_core::types::Protocol;
use livestream_media::stream::StreamCollection;

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

    SessionEnded {
        live_id: String,
        protocol: Protocol,
    },
}

impl SessionEvent {
    pub fn id(&self) -> &str {
        match self {
            SessionEvent::SessionStarted { live_id, .. } => live_id,
            SessionEvent::SessionInit { live_id, .. } => live_id,
            SessionEvent::SessionEnded { live_id, .. } => live_id,
        }
    }
}

impl Debug for SessionEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionEvent::SessionStarted { live_id, protocol } => f
                .debug_struct("SessionStarted")
                .field("live_id", live_id)
                .field("protocol", protocol)
                .finish(),
            SessionEvent::SessionInit { live_id, .. } => f
                .debug_struct("StreamInitialized")
                .field("live_id", live_id)
                .field("streams", &"<stream collection>")
                .finish(),
            SessionEvent::SessionEnded { live_id, protocol } => f
                .debug_struct("SessionEnded")
                .field("live_id", live_id)
                .field("protocol", protocol)
                .finish(),
        }
    }
}
