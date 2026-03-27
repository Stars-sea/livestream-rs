use std::sync::Arc;

use anyhow::Result;

use crate::abstraction::{MiddlewareTrait, PipeContextTrait, PipeTrait};
use crate::pipeline::Pipe;

pub struct TransformMiddleware<Input, Output>
where
    Input: PipeContextTrait,
    Output: PipeContextTrait + From<Input>,
{
    next_pipeline: Arc<Pipe<Output>>,
    _marker: std::marker::PhantomData<Input>,
}

impl<Input, Output> TransformMiddleware<Input, Output>
where
    Input: PipeContextTrait,
    Output: PipeContextTrait + From<Input>,
{
    pub fn new(next_pipeline: Arc<Pipe<Output>>) -> Self {
        Self {
            next_pipeline,
            _marker: std::marker::PhantomData,
        }
    }
}

#[async_trait::async_trait]
impl<Input, Output> MiddlewareTrait for TransformMiddleware<Input, Output>
where
    Input: PipeContextTrait,
    Output: PipeContextTrait + From<Input>,
{
    type Context = Input;

    async fn send(&self, ctx: Self::Context) -> Result<Option<Self::Context>> {
        let transformed_ctx: Output = ctx.into();

        self.next_pipeline.send(transformed_ctx).await;
        Ok(None)
    }
}
