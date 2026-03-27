use std::sync::Arc;

use crate::pipeline::Pipe;
use crate::pipeline::context::FlvPipeContext;
use crate::pipeline::middleware::BroadcastMiddleware;
use crate::pipeline::pipe::PipeFactory;

pub struct FlvPipeFactory {
    broadcast: Arc<BroadcastMiddleware<FlvPipeContext>>,
}

impl PipeFactory for FlvPipeFactory {
    type Context = FlvPipeContext;

    fn create(&self) -> Pipe<Self::Context> {
        let mut pipe = Pipe::new();

        pipe.add_middleware(self.broadcast.clone());
        // TODO: Converte to PacketPipeContext and send to persistence middleware

        pipe
    }
}
