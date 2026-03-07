pub mod lifecycle;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::Span;

use crate::ingest::events::StreamMessage;
use crate::ingest::session::lifecycle::WorkerLifecycle;
use crate::ingest::stream_info::StreamInfo;
use crate::media::output::FlvPacket;

pub struct WorkerContext {
    pub live_id: String,
    pub stop_signal: Arc<std::sync::atomic::AtomicBool>,
    pub stream_msg_tx: mpsc::UnboundedSender<(StreamMessage, Span)>,
    pub flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
    pub lifecycle: WorkerLifecycle,
    pub stream_info: Arc<StreamInfo>,
}

impl WorkerContext {
    pub fn new(
        stream_info: Arc<StreamInfo>,
        stop_signal: Arc<std::sync::atomic::AtomicBool>,
        stream_msg_tx: mpsc::UnboundedSender<(StreamMessage, Span)>,
        flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
    ) -> Self {
        let live_id = stream_info.live_id().to_string();
        Self {
            live_id: live_id.clone(),
            stop_signal,
            stream_msg_tx,
            flv_packet_tx,
            lifecycle: WorkerLifecycle::new(&live_id),
            stream_info,
        }
    }
}

pub trait StreamAdapter: Send + Sync {
    /// Runs the stream adapter logic. The implementation should monitor the `stop_signal` in the provided context
    async fn run(&mut self, ctx: &mut WorkerContext) -> anyhow::Result<()>;
}

pub struct IngestSession<T: StreamAdapter> {
    pub ctx: WorkerContext,
    pub adapter: T,
}

impl<T: StreamAdapter> IngestSession<T> {
    pub fn new(ctx: WorkerContext, adapter: T) -> Self {
        Self { ctx, adapter }
    }

    pub async fn start(mut self) {
        self.ctx
            .lifecycle
            .notify_ingest_worker_started(&self.ctx.stream_msg_tx);

        if let Err(e) = self.adapter.run(&mut self.ctx).await {
            self.ctx.lifecycle.finalize(
                &self.ctx.stream_msg_tx,
                &self.ctx.flv_packet_tx,
                Some(e.to_string()),
            );
        } else {
            self.ctx
                .lifecycle
                .finalize(&self.ctx.stream_msg_tx, &self.ctx.flv_packet_tx, None);
        }
    }
}
