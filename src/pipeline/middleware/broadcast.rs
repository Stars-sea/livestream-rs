use crossfire::{AsyncTxTrait, MAsyncTx, mpmc::List};

use anyhow::Result;

use crate::abstraction::{MiddlewareTrait, PipeContextTrait};

pub struct BroadcastMiddleware<Context>
where
    Context: PipeContextTrait + Clone + 'static + Unpin,
{
    tx: MAsyncTx<List<Context>>,
}

impl<Context> BroadcastMiddleware<Context>
where
    Context: PipeContextTrait + Clone + 'static + Unpin,
{
    pub fn new(tx: MAsyncTx<List<Context>>) -> Self {
        Self { tx }
    }
}

#[async_trait::async_trait]
impl<C> MiddlewareTrait for BroadcastMiddleware<C>
where
    C: PipeContextTrait + Clone + 'static + Unpin,
{
    type Context = C;

    async fn send(&self, ctx: Self::Context) -> Result<Option<Self::Context>> {
        self.tx.send(ctx.clone()).await?;
        Ok(Some(ctx))
    }
}
