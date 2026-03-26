use std::sync::Arc;

use anyhow::Result;

use crate::abstraction::{BusTrait, PipeContextTrait, PipeTrait};
use crate::pipeline::context::PacketPipeContext;

pub struct PipeBus {
    packet_pipe: Arc<dyn PipeTrait<Context = PacketPipeContext> + Send + Sync>,
}

impl PipeBus {
    pub fn new(packet_pipe: Arc<dyn PipeTrait<Context = PacketPipeContext> + Send + Sync>) -> Self {
        Self { packet_pipe }
    }
}

#[async_trait::async_trait]
impl BusTrait for PipeBus {
    async fn send<P, C>(&self, pipe: Arc<P>, context: C) -> Result<Option<C>>
    where
        P: PipeTrait<Context = C> + Send + Sync,
        C: PipeContextTrait,
    {
        pipe.send(context).await
    }
}
