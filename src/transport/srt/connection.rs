use anyhow::Result;
use crossfire::{MTx, mpsc::List};
use retry::OperationResult;
use retry::delay::{Exponential, jitter};
use tokio_util::sync::CancellationToken;

use crate::media::input::InputContext;
use crate::media::options::SrtInputStreamOptions;
use crate::media::packet::{Packet, PacketReadResult};
use crate::transport::message::StreamEvent;
use crate::transport::{SessionState, SrtState};

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

    pub fn run(self) -> Result<()> {
        let mut state = SrtState::Pending;

        while !self.cancel_token.is_cancelled() {
            let mut packet = Packet::alloc()?;
            match read_packet_with_retry(&self.av_ctx, &mut packet) {
                Ok(ReadResult::Ok) => {
                    if state == SrtState::Pending {
                        state = SrtState::Connected;
                        self.event_tx.send(StreamEvent::StateChange {
                            live_id: self.live_id.clone(),
                            new_state: SessionState::Srt(state),
                        })?;
                    }
                }
                Ok(ReadResult::Eof) => break,
                Err(e) => anyhow::bail!("Error reading packet: {}", e),
            }

            // TODO: Process packet and forward to media pipeline
        }

        state = SrtState::Disconnected;
        self.event_tx.send(StreamEvent::StateChange {
            live_id: self.live_id.clone(),
            new_state: SessionState::Srt(state),
        })?;
        Ok(())
    }
}

/// Read outcome abstraction for one packet pull attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadResult {
    Ok,
    Eof,
}

fn read_packet_with_retry(input_ctx: &InputContext, packet: &mut Packet) -> Result<ReadResult> {
    let result = retry::retry(
        Exponential::from_millis(10).map(jitter).take(5),
        || match packet.read(input_ctx) {
            PacketReadResult::Data => OperationResult::Ok(ReadResult::Ok),
            PacketReadResult::Eof => OperationResult::Ok(ReadResult::Eof),
            PacketReadResult::Retryable { code, message } => {
                OperationResult::Retry(anyhow::anyhow!(
                    "Retryable error reading packet: code={}, message={}",
                    code,
                    message
                ))
            }
            PacketReadResult::Fatal { code, message } => OperationResult::Err(anyhow::anyhow!(
                "Fatal error reading packet: code={}, message={}",
                code,
                message
            )),
        },
    );

    result.map_err(|e| anyhow::anyhow!("Failed to read packet after retries: {}", e))
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
