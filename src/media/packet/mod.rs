mod avpacket;
mod flv_tag;

pub use avpacket::{Packet, PacketReadResult};
pub use flv_tag::FlvTag;

#[derive(Clone, Debug)]
pub enum UnifiedPacket {
    AVPacket(Packet),
    FlvTag(FlvTag),
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

impl Into<Option<Packet>> for UnifiedPacket {
    fn into(self) -> Option<Packet> {
        match self {
            UnifiedPacket::AVPacket(pkt) => Some(pkt),
            UnifiedPacket::FlvTag(tag) => tag.into(),
        }
    }
}
