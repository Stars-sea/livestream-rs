use anyhow::Result;
use dashmap::DashMap;
use tokio::sync::broadcast;

use crate::abstraction::{MiddlewareTrait, PipeContextTrait};

pub struct BroadcastMiddleware<Context>
where
    Context: PipeContextTrait + Clone + 'static + Unpin,
{
    tx: DashMap<String, broadcast::Sender<Context>>,
}

impl<C> BroadcastMiddleware<C>
where
    C: PipeContextTrait + Clone + 'static + Unpin,
{
    pub fn new() -> Self {
        Self { tx: DashMap::new() }
    }

    pub fn subscribe(&self, stream_id: &str) -> broadcast::Receiver<C> {
        let tx = self.tx.entry(stream_id.to_string()).or_insert_with(|| {
            // Capacity is set to 1024, which can hold about 10-20 seconds of packets (depending on frame rate).
            // If a client is too slow and causes the buffer to exceed 1024, the oldest packets will be dropped,
            // and the client will receive a Lagged error.
            let (sender, _) = broadcast::channel(1024);
            sender
        });
        tx.subscribe()
    }

    pub fn remove_stream(&self, stream_id: &str) {
        self.tx.remove(stream_id);
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
        let stream_id = ctx.id();

        if let Some(tx) = self.tx.get(&stream_id) {
            tx.send(ctx.clone());
        }
        Ok(Some(ctx))
    }
}
