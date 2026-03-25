use std::sync::Arc;

use crate::abstraction::PipeTrait;
use crate::pipeline::context::PacketPipeContext;

pub struct PipeBus {
    packet_pipe: Arc<dyn PipeTrait<Context = PacketPipeContext>>,
}
