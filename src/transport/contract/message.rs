use std::sync::Arc;

use anyhow::Result;

use super::state::SessionState;
use crate::infra::media::packet::FlvTag;
use crate::infra::media::stream::StreamCollection;
use crate::queue::{ChannelSendStatus, MpscChannel};

#[derive(Debug, Clone)]
pub enum ControlMessage {
    PrecreateStream {
        live_id: String,
        passphrase: Option<String>,
    },

    StopStream {
        live_id: String,
    },
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

impl StreamEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::StateChange { .. } => "state_change",
            Self::Init { .. } => "init",
        }
    }

    pub fn live_id(&self) -> &str {
        match self {
            Self::StateChange { live_id, .. } => live_id,
            Self::Init { live_id, .. } => live_id,
        }
    }
}

pub fn send_stream_event_nonblocking(
    event_channel: &MpscChannel<StreamEvent>,
    event: StreamEvent,
    source: &'static str,
) -> Result<()> {
    let live_id = event.live_id().to_string();
    let channel = event_channel
        .clone()
        .with_source(source)
        .with_live_id(live_id);

    match channel.send(event) {
        ChannelSendStatus::Sent => Ok(()),
        ChannelSendStatus::Full => {
            anyhow::bail!("Transport event queue full at {}", source)
        }
        ChannelSendStatus::Disconnected => {
            anyhow::bail!("Transport event queue disconnected at {}", source)
        }
    }
}

pub fn send_stream_state_change(
    event_channel: &MpscChannel<StreamEvent>,
    live_id: impl Into<String>,
    new_state: SessionState,
    source: &'static str,
) -> Result<()> {
    send_stream_event_nonblocking(
        event_channel,
        StreamEvent::StateChange {
            live_id: live_id.into(),
            new_state,
        },
        source,
    )
}

pub fn send_stream_init(
    event_channel: &MpscChannel<StreamEvent>,
    live_id: impl Into<String>,
    streams: Arc<dyn StreamCollection + Send + Sync>,
    source: &'static str,
) -> Result<()> {
    send_stream_event_nonblocking(
        event_channel,
        StreamEvent::Init {
            live_id: live_id.into(),
            streams,
        },
        source,
    )
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
