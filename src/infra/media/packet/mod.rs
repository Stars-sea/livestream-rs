mod avpacket;
mod flv_tag;

use std::sync::Arc;
use std::{fmt, fmt::Debug};

pub use avpacket::{Packet, PacketReadResult};
pub use flv_tag::FlvTag;

use crate::infra::media::stream::StreamCollection;

#[derive(Clone)]
pub enum UnifiedPacket {
    AVPacket(Packet),
    FlvTag(FlvTag),
    Init(Arc<dyn StreamCollection + Send + Sync>), // For initial metadata packets that contain stream info
}

impl Debug for UnifiedPacket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnifiedPacket::AVPacket(pkt) => f.debug_tuple("AVPacket").field(pkt).finish(),
            UnifiedPacket::FlvTag(tag) => f.debug_tuple("FlvTag").field(tag).finish(),
            UnifiedPacket::Init(..) => f.write_str("Init(<stream collection>)"),
        }
    }
}

impl From<Packet> for UnifiedPacket {
    fn from(pkt: Packet) -> Self {
        UnifiedPacket::AVPacket(pkt)
    }
}

impl From<FlvTag> for UnifiedPacket {
    fn from(tag: FlvTag) -> Self {
        match tag {
            FlvTag::Audio { .. } | FlvTag::Video { .. } => UnifiedPacket::FlvTag(tag),
            FlvTag::ScriptData(meta) => UnifiedPacket::Init(Arc::new(meta)),
        }
    }
}

impl Into<Option<Packet>> for UnifiedPacket {
    fn into(self) -> Option<Packet> {
        match self {
            UnifiedPacket::AVPacket(pkt) => Some(pkt),
            UnifiedPacket::FlvTag(tag) => tag.into(),
            UnifiedPacket::Init(..) => None,
        }
    }
}
