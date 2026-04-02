use std::sync::Arc;

use super::state::SessionState;
use crate::infra::media::packet::FlvTag;
use crate::infra::media::stream::StreamCollection;

#[derive(Debug, Clone)]
pub enum ControlMessage {
    PrecreateStream {
        live_id: String,
        passphrase: Option<String>,
    },

    StopStream { live_id: String },
}

pub enum StreamEvent {
    StateChange {
        live_id: String,
        new_state: SessionState,
    },
    Init {
        live_id: String,
        streams: Arc<dyn StreamCollection + Send + Sync>,
    },
}

#[derive(Clone, Debug)]
pub struct StreamFlvTag {
    pub stream_id: String,
    pub tag: FlvTag,
}

impl StreamFlvTag {
    pub fn new(stream_id: String, tag: FlvTag) -> Self {
        Self { stream_id, tag }
    }
}
