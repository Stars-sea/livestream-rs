//! FlvSink — sends FlvTags to all RTMP/HTTP-FLV subscribers via FlvBroadcast.

use std::sync::Arc;

use anyhow::Result;
use livestream_core::{
    pad::{DemandHandle, PadReceiver},
    traits::{Node, Sink},
    types::{CodecParams, Protocol},
};
use livestream_media::flv::FlvTag;

use crate::broadcast::FlvBroadcast;

pub struct FlvSink {
    live_id: String,
    broadcast: Arc<dyn FlvBroadcast>,
    input: PadReceiver<FlvTag>,
    demand_handle: DemandHandle,
}

impl FlvSink {
    pub fn new(
        live_id: &str,
        broadcast: Arc<dyn FlvBroadcast>,
        input: PadReceiver<FlvTag>,
        demand_handle: DemandHandle,
    ) -> Self {
        Self {
            live_id: live_id.into(),
            broadcast,
            input,
            demand_handle,
        }
    }
}

impl Node for FlvSink {
    fn name(&self) -> &str {
        "flv-sink"
    }
}

#[async_trait::async_trait]
impl Sink for FlvSink {
    type Input = FlvTag;

    fn protocol(&self) -> Protocol {
        Protocol::HttpFlv
    }

    fn accepted_codec(&self) -> &[CodecParams] {
        &[]
    }

    fn input(&self) -> &PadReceiver<Self::Input> {
        &self.input
    }

    fn demand_handle(&self) -> &DemandHandle {
        &self.demand_handle
    }

    async fn consume(&self, tag: Self::Input) -> Result<()> {
        self.broadcast.broadcast(&self.live_id, tag).await
    }
}
