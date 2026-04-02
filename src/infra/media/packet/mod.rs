mod avpacket;
mod flv_tag;

use std::{fmt, fmt::Debug};

pub use avpacket::{Packet, PacketReadResult};
pub use flv_tag::FlvTag;

#[derive(Clone)]
pub enum UnifiedPacket {
    AVPacket(Packet),
    FlvTag(FlvTag),
}

impl UnifiedPacket {
    pub fn size(&self) -> usize {
        match self {
            UnifiedPacket::AVPacket(pkt) => pkt.size().max(0) as usize,
            UnifiedPacket::FlvTag(tag) => tag.payload_size(),
        }
    }
}

impl Debug for UnifiedPacket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnifiedPacket::AVPacket(pkt) => f.debug_tuple("AVPacket").field(pkt).finish(),
            UnifiedPacket::FlvTag(tag) => f.debug_tuple("FlvTag").field(tag).finish(),
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
        UnifiedPacket::FlvTag(tag)
    }
}
