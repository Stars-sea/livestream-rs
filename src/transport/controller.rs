use anyhow::Result;
use crossfire::{Tx, spsc::List};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::contract::message::ControlMessage;

pub struct TransportController {
    rtmp_tx: Tx<List<ControlMessage>>,
    srt_tx: Tx<List<ControlMessage>>,

    handle: JoinHandle<Result<()>>,

    cancel_token: CancellationToken,
}

impl TransportController {
    pub fn new(
        rtmp_tx: Tx<List<ControlMessage>>,
        srt_tx: Tx<List<ControlMessage>>,
        handle: JoinHandle<Result<()>>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            rtmp_tx,
            srt_tx,
            handle,
            cancel_token,
        }
    }

    pub async fn wait(self) -> Result<()> {
        self.handle.await??;
        Ok(())
    }

    pub fn abort(&self) {
        self.cancel_token.cancel();
        self.handle.abort();
    }
}
