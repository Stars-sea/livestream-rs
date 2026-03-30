use anyhow::Result;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub struct TransportController {
    handle: JoinHandle<Result<()>>,

    cancel_token: CancellationToken,
}

impl TransportController {
    pub fn new(handle: JoinHandle<Result<()>>, cancel_token: CancellationToken) -> Self {
        Self {
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
