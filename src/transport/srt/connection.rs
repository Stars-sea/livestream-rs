use std::sync::Arc;

use anyhow::Result;
use retry::OperationResult;
use retry::delay::{Exponential, jitter};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::infra::media::context::InputContext;
use crate::infra::media::options::SrtInputStreamOptions;
use crate::infra::media::packet::{Packet, PacketReadResult};
use crate::infra::media::stream::StaticStreamCollection;
use crate::pipeline::{PipeBus, UnifiedPacketContext};
use crate::queue::MpscChannel;
use crate::transport::contract::message::{
    StreamEvent, send_stream_init, send_stream_state_change,
};
use crate::transport::contract::state::{SessionState, SrtState};

pub struct SrtConnection {
    stream_id: String,

    av_ctx: InputContext,

    event_channel: MpscChannel<StreamEvent>,
    bus: PipeBus,
    runtime_handle: Handle,

    cancel_token: CancellationToken,
}

pub struct SrtConnectionBuilder {
    port: u16,

    stream_id: String,
    passphrase: Option<String>,

    event_channel: MpscChannel<StreamEvent>,
    bus: PipeBus,
    runtime_handle: Handle,
}

impl SrtConnection {
    fn new(
        stream_id: String,
        av_ctx: InputContext,
        event_channel: MpscChannel<StreamEvent>,
        bus: PipeBus,
        runtime_handle: Handle,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            stream_id,
            event_channel,
            av_ctx,
            bus,
            runtime_handle,
            cancel_token,
        }
    }

    fn emit_state_change(&self, state: SrtState) -> Result<()> {
        send_stream_state_change(
            &self.event_channel,
            self.stream_id.clone(),
            SessionState::Srt(state),
            "srt.connection.emit_state_change",
        )?;
        Ok(())
    }

    fn on_init(&self) -> Result<()> {
        let init_streams = Arc::new(
            StaticStreamCollection::from_streams(&self.av_ctx)
                .map_err(|e| anyhow::anyhow!("Failed to snapshot SRT stream info: {}", e))?,
        );
        send_stream_init(
            &self.event_channel,
            self.stream_id.clone(),
            init_streams,
            "srt.connection.on_init",
        )?;

        Ok(())
    }

    fn handle_data_packet(&self, state: &mut SrtState, packet: Packet) -> Result<()> {
        if *state == SrtState::Pending {
            self.on_init()?;
            *state = SrtState::Connected;
            self.emit_state_change(*state)?;
        }

        let context = UnifiedPacketContext::new(
            self.stream_id.clone(),
            packet.into(),
            self.cancel_token.clone(),
        );
        self.runtime_handle
            .block_on(self.bus.send_packet(context))
            .map_err(|e| anyhow::anyhow!("Failed to send SRT packet to pipeline: {}", e))?;

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
        event_channel: MpscChannel<StreamEvent>,
        bus: PipeBus,
        runtime_handle: Handle,
    ) -> Self {
        Self {
            port,
            stream_id,
            passphrase,
            event_channel,
            bus,
            runtime_handle,
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
            self.event_channel,
            self.bus,
            self.runtime_handle,
            cancel_token,
        ))
    }
}
