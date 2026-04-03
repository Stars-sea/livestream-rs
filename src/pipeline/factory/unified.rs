use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossfire::MTx;
use crossfire::mpsc::List;

use crate::infra::media::stream::StreamCollection;
use crate::pipeline::Pipe;
use crate::pipeline::UnifiedPacketContext;
use crate::pipeline::middleware::{FlvMuxForwardMiddleware, OTelMiddleware, SegmentMiddleware};
use crate::pipeline::pipe::PipeFactory;
use crate::transport::contract::message::StreamFlvTag;

#[derive(Clone)]
pub struct UnifiedPipeFactory {
    rtmp_tag_tx: MTx<List<StreamFlvTag>>,
    segment_duration: Duration,
}

impl UnifiedPipeFactory {
    pub fn new(segment_duration: Duration, rtmp_tag_tx: MTx<List<StreamFlvTag>>) -> Self {
        Self {
            rtmp_tag_tx,
            segment_duration,
        }
    }
}

impl PipeFactory for UnifiedPipeFactory {
    type Context = UnifiedPacketContext;
    type Args = Arc<dyn StreamCollection + Send + Sync>;

    fn create(&self, id: String, args: Self::Args) -> Result<Pipe<Self::Context>> {
        let mut pipe = Pipe::new();
        pipe.add_middleware(Arc::new(OTelMiddleware::new(id.clone())));
        pipe.add_middleware(Arc::new(FlvMuxForwardMiddleware::new(
            id.clone(),
            args.clone(),
            self.rtmp_tag_tx.clone(),
        )?));
        pipe.add_middleware(Arc::new(SegmentMiddleware::new(
            id,
            args,
            self.segment_duration,
        )?));
        Ok(pipe)
    }
}
