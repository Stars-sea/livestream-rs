use super::state::PipeState;

pub trait PipeContextTrait {
    type Payload;

    fn id(&self) -> String;

    fn state(&self) -> PipeState;

    fn payload(&self) -> Self::Payload;
}
