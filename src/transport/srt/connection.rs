use anyhow::Result;
use crossfire::{MTx, mpsc::List};
use tokio_util::sync::CancellationToken;

use crate::media::input::InputContext;
use crate::media::options::SrtInputStreamOptions;
use crate::transport::message::StreamEvent;

pub struct SrtConnection {
    live_id: String,

    av_ctx: InputContext,

    event_tx: MTx<List<StreamEvent>>,

    cancel_token: CancellationToken,
}

pub struct SrtConnectionBuilder {
    host: String,
    port: u16,

    live_id: String,
    passphrase: String,

    event_tx: MTx<List<StreamEvent>>,
    cancel_token: CancellationToken,
}

impl SrtConnection {
    fn new(
        live_id: String,
        av_ctx: InputContext,
        event_tx: MTx<List<StreamEvent>>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            live_id,
            event_tx,
            av_ctx,
            cancel_token,
        }
    }

    pub fn run(self) {
        while !self.cancel_token.is_cancelled() {}
    }
}

impl SrtConnectionBuilder {
    pub fn new(
        host: String,
        port: u16,
        live_id: String,
        passphrase: String,
        event_tx: MTx<List<StreamEvent>>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            host,
            port,
            live_id,
            passphrase,
            event_tx,
            cancel_token,
        }
    }

    pub fn live_id(&self) -> String {
        self.live_id.clone()
    }

    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    pub fn build(self) -> Result<SrtConnection> {
        let options = SrtInputStreamOptions::new(
            self.host.clone(),
            self.port,
            self.live_id.clone(),
            self.passphrase.clone(),
        );

        let av_ctx = InputContext::open(&options, self.cancel_token.clone())?;
        Ok(SrtConnection::new(
            self.live_id,
            av_ctx,
            self.event_tx,
            self.cancel_token,
        ))
    }
}
