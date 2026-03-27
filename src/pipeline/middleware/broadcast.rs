use crossfire::{AsyncTxTrait, MAsyncTx, mpsc::List};

use anyhow::Result;
use dashmap::DashMap;

use crate::abstraction::{MiddlewareTrait, PipeContextTrait};

pub struct BroadcastMiddleware<Context>
where
    Context: PipeContextTrait + Clone + 'static + Unpin,
{
    tx: DashMap<String, MAsyncTx<List<Context>>>,
}

impl<C> BroadcastMiddleware<C>
where
    C: PipeContextTrait + Clone + 'static + Unpin,
{
    pub fn new() -> Self {
        Self { tx: DashMap::new() }
    }

    pub fn add_client(&self, id: String, tx: MAsyncTx<List<C>>) {
        self.tx.insert(id, tx);
    }
}

impl<C> Default for BroadcastMiddleware<C>
where
    C: PipeContextTrait + Clone + 'static + Unpin,
{
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl<C> MiddlewareTrait for BroadcastMiddleware<C>
where
    C: PipeContextTrait + Clone + 'static + Unpin,
{
    type Context = C;

    async fn send(&self, ctx: Self::Context) -> Result<Option<Self::Context>> {
        self.tx.retain(|_, v| !v.is_disconnected());

        if let Some(tx) = self.tx.get(&ctx.id()) {
            tx.send(ctx.clone()).await?;
        }
        Ok(Some(ctx))
    }
}
