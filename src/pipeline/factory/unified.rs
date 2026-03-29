use std::sync::Arc;

use crate::pipeline::Pipe;
use crate::pipeline::UnifiedPacketContext;
use crate::pipeline::middleware::BroadcastMiddleware;
use crate::pipeline::pipe::PipeFactory;

pub struct UnifiedPipeFactory {
    broadcast: Arc<BroadcastMiddleware<UnifiedPacketContext>>,
}

impl PipeFactory for UnifiedPipeFactory {
    type Context = UnifiedPacketContext;

    fn create(&self) -> Pipe<Self::Context> {
        let mut pipe = Pipe::new();

        pipe.add_middleware(self.broadcast.clone());

        pipe
    }
}
