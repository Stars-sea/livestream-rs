use std::sync::Arc;

use anyhow::Result;
use retry::delay::{Exponential, jitter};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

use crate::channel::{self, SpscTx};
use crate::infra::media::context::InputContext;
use crate::infra::media::options::SrtInputStreamOptions;
use crate::infra::media::packet::{Packet, PacketReadResult};
use crate::infra::media::stream::StaticStreamCollection;
use crate::pipeline::{PipeBus, UnifiedPacketContext};
use crate::transport::lifecycle::HandlerLifecycle;

pub struct SrtConnection {
    stream_id: String,

    av_ctx: InputContext,

    bus: PipeBus,

    cancel_token: CancellationToken,
}

pub struct SrtConnectionBuilder {
    port: u16,

    stream_id: String,
    passphrase: Option<String>,

    bus: PipeBus,
}

impl SrtConnection {
    fn new(
        stream_id: String,
        av_ctx: InputContext,
        bus: PipeBus,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            stream_id,
            av_ctx,
            bus,
            cancel_token,
        }
    }

    pub async fn run(self, lifecycle: HandlerLifecycle) -> Result<()> {
        let streams = StaticStreamCollection::from_streams(&self.av_ctx)?;
        if !lifecycle.initialized() {
            lifecycle.init(Arc::new(streams)).await;
        }

        let live_id = self.stream_id.clone();
        let live_id_for_log = live_id.clone();
        let cancel_token = self.cancel_token.clone();

        let (tx, mut rx) = channel::spsc("srt-relay", Some(live_id.clone()), 1024);
        tokio::task::spawn_blocking(move || {
            read_packet_loop(live_id_for_log, self.av_ctx, tx, cancel_token)
        });

        if let Err(e) = lifecycle.connected().await {
            error!("Error during lifecycle connected callback: {}", e);
        }

        while let Some(packet) = rx.next().await {
            let context =
                UnifiedPacketContext::new(&live_id, packet.into(), self.cancel_token.clone());
            if let Err(e) = self.bus.send_packet(context).await {
                error!("Error during lifecycle packet_sent callback: {}", e);
            }
        }

        Ok(())
    }
}

fn read_packet_loop(
    live_id: impl Into<String>,
    input_ctx: InputContext,
    sender: SpscTx<Packet>,
    ct: CancellationToken,
) {
    let live_id = live_id.into();
    while !ct.is_cancelled() {
        let mut packet = match Packet::alloc() {
            Ok(p) => p,
            Err(e) => {
                error!("Failed to allocate packet: {}, retrying...", e);
                break;
            }
        };

        let mut read_succeed = false;
        let mut retry_exponential = Exponential::from_millis(10).map(jitter).take(5);
        while let Some(duration) = retry_exponential.next() {
            match packet.read(&input_ctx) {
                PacketReadResult::Data => {
                    read_succeed = true;
                    break;
                }
                PacketReadResult::Eof => {
                    debug!(
                        live_id = live_id,
                        "End of stream reached, stopping packet read loop"
                    );
                    return;
                }
                PacketReadResult::Retryable { code, message } => {
                    error!(
                        live_id = live_id,
                        code = code,
                        message = message,
                        "Retryable error reading packet: retrying in {:?}",
                        duration
                    );
                    std::thread::sleep(duration);
                    continue;
                }
                PacketReadResult::Fatal { code, message } => {
                    error!(
                        live_id = live_id,
                        code = code,
                        message = message,
                        "Fatal error reading packet: aborting"
                    );
                    return;
                }
            }
        }

        if !read_succeed {
            error!(
                live_id = live_id,
                "Failed to read packet after retries, aborting read loop"
            );
            continue;
        }

        if let Err(e) = sender.send(packet) {
            error!(
                live_id = live_id,
                "Failed to send packet to channel: {}, dropping packet", e
            );
        }
    }
}

impl SrtConnectionBuilder {
    pub fn new(port: u16, stream_id: String, passphrase: Option<String>, bus: PipeBus) -> Self {
        Self {
            port,
            stream_id,
            passphrase,
            bus,
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
            self.bus,
            cancel_token,
        ))
    }
}
