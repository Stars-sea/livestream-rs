use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::media::output::FlvPacket;
use crate::ingest::adapters::rtmp::RtmpTag;
use crate::ingest::stream_info::StreamInfo;

pub trait StreamRegistry: Debug + Send + Sync {
    fn get_stream<'a>(
        &'a self,
        live_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<Arc<StreamInfo>>> + Send + 'a>>;

    fn get_rtmp_tx<'a>(
        &'a self,
        _live_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<mpsc::UnboundedSender<RtmpTag>>> + Send + 'a>> {
        Box::pin(async { None })
    }
}

pub trait MediaBus: Debug + Send + Sync {
    fn sender(&self) -> mpsc::UnboundedSender<FlvPacket>;
}

#[derive(Debug, Clone)]
pub struct FlvPacketBus {
    tx: mpsc::UnboundedSender<FlvPacket>,
}

impl FlvPacketBus {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<FlvPacket>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }
}

impl MediaBus for FlvPacketBus {
    fn sender(&self) -> mpsc::UnboundedSender<FlvPacket> {
        self.tx.clone()
    }
}
