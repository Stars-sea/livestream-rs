use anyhow::Result;
use crossfire::{AsyncRx, spsc::List};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::transport::message::ControlMessage;

pub struct SrtServer {
    rx: AsyncRx<List<ControlMessage>>,

    cancel_token: CancellationToken,
}

impl SrtServer {
    pub fn new(rx: AsyncRx<List<ControlMessage>>, cancel_token: CancellationToken) -> Self {
        Self { rx, cancel_token }
    }

    pub async fn run(&self) -> Result<()> {
        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    debug!("RTMP server cancellation requested, shutting down");
                    break;
                }

                msg = self.rx.recv() => {
                    // TODO: Handle control messages from the SRT connection
                }
            }
        }

        Ok(())
    }
}
