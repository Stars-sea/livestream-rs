use tokio_util::sync::CancellationToken;

use crate::abstraction::PipeContextTrait;
use crate::media::format::FlvTag;

#[derive(Clone)]
pub struct FlvPipeContext {
    pub live_id: String,

    pub tag: FlvTag,

    pub cancel_token: CancellationToken,
}

impl PipeContextTrait for FlvPipeContext {
    type Payload = FlvTag;

    fn id(&self) -> String {
        self.live_id.clone()
    }

    fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    fn payload(&self) -> &Self::Payload {
        &self.tag
    }
}
