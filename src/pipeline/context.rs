use tokio_util::sync::CancellationToken;

use crate::abstraction::PipeContextTrait;
use crate::infra::media::packet::{Packet, UnifiedPacket};

#[derive(Clone, Debug)]
pub struct UnifiedPacketContext {
    id: String,
    packet: UnifiedPacket,

    cancel_token: CancellationToken,
}

impl UnifiedPacketContext {
    pub fn new(id: String, packet: UnifiedPacket, cancel_token: CancellationToken) -> Self {
        Self {
            id,
            packet,
            cancel_token,
        }
    }
}

impl PipeContextTrait for UnifiedPacketContext {
    type Payload = UnifiedPacket;

    fn id(&self) -> String {
        self.id.clone()
    }

    fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    fn payload(&self) -> &Self::Payload {
        &self.packet
    }
}

#[derive(Clone, Debug)]
pub struct AVPacketContext {
    id: String,
    packet: Packet,

    cancel_token: CancellationToken,
}

impl AVPacketContext {
    pub fn new(id: String, packet: Packet, cancel_token: CancellationToken) -> Self {
        Self {
            id,
            packet,
            cancel_token,
        }
    }
}

impl PipeContextTrait for AVPacketContext {
    type Payload = Packet;

    fn id(&self) -> String {
        self.id.clone()
    }

    fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    fn payload(&self) -> &Self::Payload {
        &self.packet
    }
}

impl Into<UnifiedPacketContext> for AVPacketContext {
    fn into(self) -> UnifiedPacketContext {
        UnifiedPacketContext::new(self.id, self.packet.into(), self.cancel_token)
    }
}

impl Into<Option<AVPacketContext>> for UnifiedPacketContext {
    fn into(self) -> Option<AVPacketContext> {
        let packet: Option<Packet> = self.packet.try_into().ok();
        packet.map(|pkt| AVPacketContext::new(self.id, pkt, self.cancel_token))
    }
}
