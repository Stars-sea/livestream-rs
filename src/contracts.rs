use std::fmt::Debug;
use std::sync::Arc;

use crossfire::{AsyncRx, MTx, mpsc};

use crate::ingest::StreamInfo;
use crate::ingest::adapters::RtmpTag;
use crate::media::output::FlvPacket;

#[async_trait::async_trait]
pub trait StreamRegistry: Debug + Send + Sync {
    async fn get_stream(&self, live_id: &str) -> Option<Arc<StreamInfo>>;

    async fn get_rtmp_tx(&self, _live_id: &str) -> Option<MTx<mpsc::List<RtmpTag>>> {
        None
    }
}

pub trait MediaBus: Debug + Send + Sync {
    fn sender(&self) -> MTx<mpsc::List<FlvPacket>>;
}

#[derive(Debug, Clone)]
pub struct FlvPacketBus {
    tx: MTx<mpsc::List<FlvPacket>>,
}

impl FlvPacketBus {
    pub fn new() -> (Self, AsyncRx<mpsc::List<FlvPacket>>) {
        let (tx, rx) = mpsc::unbounded_async();
        (Self { tx }, rx)
    }
}

impl MediaBus for FlvPacketBus {
    fn sender(&self) -> MTx<mpsc::List<FlvPacket>> {
        self.tx.clone()
    }
}
