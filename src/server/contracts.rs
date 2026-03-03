use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;

use tokio::sync::mpsc;

use crate::core::output::FlvPacket;

pub trait StreamRegistry: Debug + Send + Sync {
    fn has_stream<'a>(
        &'a self,
        live_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;
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
