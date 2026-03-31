use crate::infra::media::packet::Packet;

pub struct WrappedPacket {
    pub stream_id: String,
    pub packet: Packet,
}

impl WrappedPacket {
    pub fn new(stream_id: &str, packet: Packet) -> Self {
        Self {
            stream_id: stream_id.to_string(),
            packet,
        }
    }
}
