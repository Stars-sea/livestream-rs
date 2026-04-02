use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::infra::media::StaticStreamCollection;
use crate::infra::media::packet::Packet;

pub enum WrappedPacketPayload {
    Init(Arc<StaticStreamCollection>),
    Packet(Packet),
}

pub struct WrappedPacket {
    pub stream_id: String,
    pub payload: WrappedPacketPayload,
    pub cancel_token: CancellationToken,
}

impl WrappedPacket {
    pub fn init(
        stream_id: &str,
        streams: Arc<StaticStreamCollection>,
        cancel_token: &CancellationToken,
    ) -> Self {
        Self {
            stream_id: stream_id.to_string(),
            payload: WrappedPacketPayload::Init(streams),
            cancel_token: cancel_token.clone(),
        }
    }

    pub fn packet(stream_id: &str, packet: Packet, cancel_token: &CancellationToken) -> Self {
        Self {
            stream_id: stream_id.to_string(),
            payload: WrappedPacketPayload::Packet(packet),
            cancel_token: cancel_token.clone(),
        }
    }
}
