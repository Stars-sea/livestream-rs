use tokio_util::sync::CancellationToken;

use crate::infra::media::packet::Packet;

pub struct WrappedPacket {
    pub stream_id: String,
    pub packet: Packet,
    pub cancel_token: CancellationToken,
}

impl WrappedPacket {
    pub fn new(stream_id: String, packet: Packet, cancel_token: CancellationToken) -> Self {
        Self {
            stream_id,
            packet,
            cancel_token,
        }
    }
}
