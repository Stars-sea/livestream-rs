use std::sync::Arc;

use anyhow::Result;

use super::Pipe;
use super::context::PacketPipeContext;
use crate::abstraction::PipeTrait;

pub struct PipeBus {
    packet_pipe: Arc<Pipe<PacketPipeContext>>,
}

impl PipeBus {
    pub fn new(packet_pipe: Arc<Pipe<PacketPipeContext>>) -> Self {
        Self { packet_pipe }
    }

    pub async fn send_packet(
        &self,
        context: PacketPipeContext,
    ) -> Result<Option<PacketPipeContext>> {
        self.packet_pipe.send(context).await
    }
}
