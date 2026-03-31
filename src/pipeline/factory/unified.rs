use std::sync::Arc;

use crate::pipeline::Pipe;
use crate::pipeline::UnifiedPacketContext;
use crate::pipeline::middleware::{BroadcastMiddleware, SegmentMiddleware};
use crate::pipeline::pipe::PipeFactory;

pub struct UnifiedPipeFactory {
    broadcast: Arc<BroadcastMiddleware<UnifiedPacketContext>>,
    segment: Arc<SegmentMiddleware>,
}

impl UnifiedPipeFactory {
    pub fn new(
        broadcast: Arc<BroadcastMiddleware<UnifiedPacketContext>>,
        segment: Arc<SegmentMiddleware>,
    ) -> Self {
        Self { broadcast, segment }
    }
}

impl PipeFactory for UnifiedPipeFactory {
    type Context = UnifiedPacketContext;

    fn create(&self) -> Pipe<Self::Context> {
        let mut pipe = Pipe::new();

        pipe.add_middleware(self.segment.clone());
        pipe.add_middleware(self.broadcast.clone());

        pipe
    }
}
