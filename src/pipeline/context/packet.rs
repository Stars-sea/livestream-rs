use tokio_util::sync::CancellationToken;

use crate::abstraction::PipeContextTrait;
use crate::media::Packet;

pub struct PacketPipeContext {
    id: String,
    cancel_token: CancellationToken,

    payload: Packet,
}

impl PipeContextTrait for PacketPipeContext {
    type Payload = Packet;

    fn id(&self) -> String {
        self.id.clone()
    }

    fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    fn payload(&self) -> &Self::Payload {
        &self.payload
    }
}
