use tokio_util::sync::CancellationToken;

use crate::abstraction::PipeContextTrait;
use crate::infra::media::packet::UnifiedPacket;

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
