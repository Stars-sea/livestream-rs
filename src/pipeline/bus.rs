use std::sync::Arc;

use anyhow::Result;

use super::{Pipe, UnifiedPacketContext};
use crate::abstraction::PipeTrait;

#[derive(Clone)]
pub struct PipeBus {
    packet_pipe: Arc<Pipe<UnifiedPacketContext>>,
}

impl PipeBus {
    pub fn new(packet_pipe: Arc<Pipe<UnifiedPacketContext>>) -> Self {
        Self { packet_pipe }
    }

    pub async fn send_packet(&self, context: UnifiedPacketContext) -> Result<UnifiedPacketContext> {
        self.packet_pipe.send(context).await
    }
}
