use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crossfire::{AsyncRx, MTx, mpsc};

use crate::ingest::adapters::rtmp::RtmpTag;
use crate::ingest::stream_info::StreamInfo;
use crate::media::output::FlvPacket;

pub trait StreamRegistry: Debug + Send + Sync {
    fn get_stream<'a>(
        &'a self,
        live_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<Arc<StreamInfo>>> + Send + 'a>>;

    fn get_rtmp_tx<'a>(
        &'a self,
        _live_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<MTx<mpsc::List<RtmpTag>>>> + Send + 'a>> {
        Box::pin(async { None })
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
