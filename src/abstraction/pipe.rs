use anyhow::Result;

use super::context::PipeContextTrait;

pub trait PipeTrait {
    type Context: PipeContextTrait;

    type NextPipe: PipeTrait;

    fn send(&self, context: Self::Context) -> Result<()>;

    fn next(&self) -> Option<Self::NextPipe>;
}
