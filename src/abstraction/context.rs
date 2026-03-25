use tokio_util::sync::CancellationToken;

pub trait PipeContextTrait: Send + Sync {
    type Payload: Send + Sync;

    fn id(&self) -> String;

    fn cancel_token(&self) -> CancellationToken;

    fn payload(&self) -> &Self::Payload;
}
