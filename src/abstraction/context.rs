use std::sync::Arc;

use tokio_util::sync::CancellationToken;

pub trait PipeContextTrait: Send + Sync {
    type Payload: Send + Sync;

    fn id(&self) -> &str;

    fn cancel_token(&self) -> CancellationToken;

    fn payload(&self) -> &Self::Payload;
}

impl<C> PipeContextTrait for Arc<C>
where
    C: PipeContextTrait,
{
    type Payload = C::Payload;

    fn id(&self) -> &str {
        self.as_ref().id()
    }

    fn cancel_token(&self) -> CancellationToken {
        self.as_ref().cancel_token()
    }

    fn payload(&self) -> &Self::Payload {
        self.as_ref().payload()
    }
}

impl<C> PipeContextTrait for Box<C>
where
    C: PipeContextTrait,
{
    type Payload = C::Payload;

    fn id(&self) -> &str {
        self.as_ref().id()
    }

    fn cancel_token(&self) -> CancellationToken {
        self.as_ref().cancel_token()
    }

    fn payload(&self) -> &Self::Payload {
        self.as_ref().payload()
    }
}
