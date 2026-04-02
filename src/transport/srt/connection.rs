use std::sync::Arc;

use anyhow::Result;
use crossfire::{MTx, mpsc::List};
use retry::OperationResult;
use retry::delay::{Exponential, jitter};
use tokio_util::sync::CancellationToken;

use crate::infra::media::StaticStreamCollection;
use crate::infra::media::context::InputContext;
use crate::infra::media::options::SrtInputStreamOptions;
use crate::infra::media::packet::{Packet, PacketReadResult};
use crate::transport::contract::message::StreamEvent;
use crate::transport::contract::state::{SessionState, SrtState};
use crate::transport::srt::packet::WrappedPacket;

pub struct SrtConnection {
    live_id: String,

    av_ctx: InputContext,

    event_tx: MTx<List<StreamEvent>>,
    packet_tx: MTx<List<WrappedPacket>>,

    cancel_token: CancellationToken,
}

pub struct SrtConnectionBuilder {
    host: String,
    port: u16,

    live_id: String,
    passphrase: String,

    packet_tx: MTx<List<WrappedPacket>>,
    event_tx: MTx<List<StreamEvent>>,
}

impl SrtConnection {
    fn new(
        live_id: String,
        av_ctx: InputContext,
        event_tx: MTx<List<StreamEvent>>,
        packet_tx: MTx<List<WrappedPacket>>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            live_id,
            event_tx,
            av_ctx,
            packet_tx,
            cancel_token,
        }
    }

    fn emit_state_change(&self, state: SrtState) -> Result<()> {
        self.event_tx.send(StreamEvent::StateChange {
            live_id: self.live_id.clone(),
            new_state: SessionState::Srt(state),
        })?;
        Ok(())
    }

    fn on_init(&self) -> Result<()> {
        let init_streams = Arc::new(
            StaticStreamCollection::from_streams(&self.av_ctx)
                .map_err(|e| anyhow::anyhow!("Failed to snapshot SRT stream info: {}", e))?,
        );
        self.packet_tx.send(WrappedPacket::init(
            &self.live_id,
            init_streams,
            &self.cancel_token,
        ))?;

        Ok(())
    }

    pub fn run(self) -> Result<()> {
        let mut state = SrtState::Pending;

        while !self.cancel_token.is_cancelled() {
            let mut packet = Packet::alloc()?;
            match read_packet_with_retry(&self.av_ctx, &mut packet) {
                Ok(ReadResult::Ok) => {
                    if state == SrtState::Pending {
                        self.on_init()?;
                        state = SrtState::Connected;
                        self.emit_state_change(state)?;
                    }

                    self.packet_tx.send(WrappedPacket::packet(
                        &self.live_id,
                        packet,
                        &self.cancel_token,
                    ))?;
                }
                Ok(ReadResult::Eof) => break,
                Err(e) => anyhow::bail!("Error reading packet: {}", e),
            }
        }

        state = SrtState::Disconnected;
        self.emit_state_change(state)?;
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
        packet_tx: MTx<List<WrappedPacket>>,
        event_tx: MTx<List<StreamEvent>>,
    ) -> Self {
        Self {
            host,
            port,
            live_id,
            passphrase,
            packet_tx,
            event_tx,
        }
    }

    pub fn live_id(&self) -> &str {
        &self.live_id
    }

    pub fn build(self, cancel_token: CancellationToken) -> Result<SrtConnection> {
        let options = SrtInputStreamOptions::new(
            self.host.clone(),
            self.port,
            self.live_id.clone(),
            self.passphrase.clone(),
        );

        let av_ctx = InputContext::open(&options, cancel_token.clone())?;
        Ok(SrtConnection::new(
            self.live_id,
            av_ctx,
            self.event_tx,
            self.packet_tx,
            cancel_token,
        ))
    }
}
