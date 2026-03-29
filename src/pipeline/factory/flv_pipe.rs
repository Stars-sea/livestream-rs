use std::sync::Arc;

use crate::pipeline::Pipe;
use crate::pipeline::UnifiedPacketContext;
use crate::pipeline::middleware::BroadcastMiddleware;
use crate::pipeline::pipe::PipeFactory;

pub struct FlvPipeFactory {
    broadcast: Arc<BroadcastMiddleware<UnifiedPacketContext>>,
}

impl PipeFactory for FlvPipeFactory {
    type Context = UnifiedPacketContext;

    fn create(&self) -> Pipe<Self::Context> {
        let mut pipe = Pipe::new();

        pipe.add_middleware(self.broadcast.clone());
        // TODO: Convert to UnifiedPacketContext and send to persistence middleware

        pipe
    }
}
