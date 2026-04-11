use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use crate::infra::media::stream::StreamCollection;
use crate::pipeline::Pipe;
use crate::pipeline::UnifiedPacketContext;
use crate::pipeline::middleware::{FlvMuxForwardMiddleware, OTelMiddleware, SegmentMiddleware};
use crate::pipeline::pipe::PipeFactory;
use crate::queue::MpscChannel;
use crate::transport::contract::message::StreamFlvTag;

#[derive(Clone)]
pub struct UnifiedPipeFactory {
    rtmp_tag_channel: MpscChannel<StreamFlvTag>,
    segment_duration: Duration,
    segment_cachedir: String,
    flv_relay_queue_capacity: usize,
}

impl UnifiedPipeFactory {
    pub fn new(
        segment_duration: Duration,
        segment_cachedir: String,
        flv_relay_queue_capacity: usize,
        rtmp_tag_channel: MpscChannel<StreamFlvTag>,
    ) -> Self {
        Self {
            rtmp_tag_channel,
            segment_duration,
            segment_cachedir,
            flv_relay_queue_capacity,
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
            self.flv_relay_queue_capacity,
            self.rtmp_tag_channel.clone(),
        )?));
        pipe.add_middleware(Arc::new(SegmentMiddleware::new(
            id,
            args,
            self.segment_duration,
            self.segment_cachedir.clone(),
        )?));
        Ok(pipe)
    }
}
