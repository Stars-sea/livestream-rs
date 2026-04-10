use std::sync::Arc;

use anyhow::Result;
use crossfire::{MTx, mpsc::Array};
use retry::OperationResult;
use retry::delay::{Exponential, jitter};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::infra::media::context::InputContext;
use crate::infra::media::options::SrtInputStreamOptions;
use crate::infra::media::packet::{Packet, PacketReadResult};
use crate::infra::media::stream::StaticStreamCollection;
use crate::transport::contract::message::StreamEvent;
use crate::transport::contract::state::{SessionState, SrtState};
use crate::transport::srt::packet::WrappedPacket;

pub struct SrtConnection {
    stream_id: String,

    av_ctx: InputContext,

    event_tx: MTx<Array<StreamEvent>>,
    packet_tx: MTx<Array<WrappedPacket>>,

    cancel_token: CancellationToken,
}

pub struct SrtConnectionBuilder {
    port: u16,

    stream_id: String,
    passphrase: Option<String>,

    packet_tx: MTx<Array<WrappedPacket>>,
    event_tx: MTx<Array<StreamEvent>>,
}

impl SrtConnection {
    fn new(
        stream_id: String,
        av_ctx: InputContext,
        event_tx: MTx<Array<StreamEvent>>,
        packet_tx: MTx<Array<WrappedPacket>>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            stream_id,
            event_tx,
            av_ctx,
            packet_tx,
            cancel_token,
        }
    }

    fn emit_state_change(&self, state: SrtState) -> Result<()> {
        self.event_tx.send(StreamEvent::StateChange {
            live_id: self.stream_id.clone(),
            new_state: SessionState::Srt(state),
        })?;
        Ok(())
    }

    fn on_init(&self) -> Result<()> {
        let init_streams = Arc::new(
            StaticStreamCollection::from_streams(&self.av_ctx)
                .map_err(|e| anyhow::anyhow!("Failed to snapshot SRT stream info: {}", e))?,
        );
        self.event_tx.send(StreamEvent::Init {
            live_id: self.stream_id.clone(),
            streams: init_streams,
        })?;

        Ok(())
    }

    fn handle_data_packet(&self, state: &mut SrtState, packet: Packet) -> Result<()> {
        if *state == SrtState::Pending {
            self.on_init()?;
            *state = SrtState::Connected;
            self.emit_state_change(*state)?;
        }

        self.packet_tx.send(WrappedPacket::new(
            self.stream_id.clone(),
            packet,
            self.cancel_token.clone(),
        ))?;

        Ok(())
    }

    fn run_loop(&self, state: &mut SrtState) -> Result<()> {
        while !self.cancel_token.is_cancelled() {
            let mut packet = Packet::alloc()?;
            match read_packet_with_retry(&self.av_ctx, &mut packet) {
                Ok(ReadResult::Ok) => self.handle_data_packet(state, packet)?,
                Ok(ReadResult::Eof) => break,
                Err(e) => anyhow::bail!("Error reading packet: {}", e),
            }
        }

        Ok(())
    }

    pub fn run(self) -> Result<()> {
        let mut state = SrtState::Pending;
        let run_result = self.run_loop(&mut state);

        if let Err(e) = self.emit_state_change(SrtState::Disconnected) {
            if run_result.is_ok() {
                return Err(e);
            }

            warn!(stream_id = %self.stream_id, error = %e, "Failed to emit SRT disconnected state after run error");
        }

        run_result
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
        port: u16,
        stream_id: String,
        passphrase: Option<String>,
        packet_tx: MTx<Array<WrappedPacket>>,
        event_tx: MTx<Array<StreamEvent>>,
    ) -> Self {
        Self {
            port,
            stream_id,
            passphrase,
            packet_tx,
            event_tx,
        }
    }

    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    pub fn build(self, cancel_token: CancellationToken) -> Result<SrtConnection> {
        let options =
            SrtInputStreamOptions::new(self.port, self.stream_id.clone(), self.passphrase);

        let av_ctx = InputContext::open(&options, cancel_token.clone())?;
        Ok(SrtConnection::new(
            self.stream_id,
            av_ctx,
            self.event_tx,
            self.packet_tx,
            cancel_token,
        ))
    }
}
