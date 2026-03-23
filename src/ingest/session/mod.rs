pub mod lifecycle;
use crossfire::{MTx, mpsc};
use std::sync::Arc;
use tracing::Span;

use crate::ingest::events::StreamMessage;
use crate::ingest::session::lifecycle::WorkerLifecycle;
use crate::ingest::stream_info::StreamInfo;
use crate::media::output::FlvPacket;

/// Runtime dependency container injected into a single adapter run.
///
/// Responsibilities:
/// - Provide immutable identity/config (`live_id`, `stream_info`).
/// - Provide control and communication primitives (`stop_signal`, channels).
/// - Provide lifecycle helper for worker-level event deduplication.
///
/// Out of scope:
/// - No protocol-specific ingest behavior.
/// - No global registry/resource management.
pub struct WorkerContext {
    pub live_id: String,
    pub stop_signal: Arc<std::sync::atomic::AtomicBool>,
    pub flv_packet_tx: MTx<mpsc::List<FlvPacket>>,
    pub lifecycle: WorkerLifecycle,
    pub stream_info: Arc<StreamInfo>,
}

impl WorkerContext {
    pub fn new(
        stream_info: Arc<StreamInfo>,
        stop_signal: Arc<std::sync::atomic::AtomicBool>,
        stream_msg_tx: MTx<mpsc::List<(StreamMessage, Span)>>,
        flv_packet_tx: MTx<mpsc::List<FlvPacket>>,
    ) -> Self {
        let live_id = stream_info.live_id().to_string();
        Self {
            live_id: live_id.clone(),
            stop_signal,
            flv_packet_tx,
            lifecycle: WorkerLifecycle::new(&live_id, stream_msg_tx),
            stream_info,
        }
    }
}

/// Protocol adapter contract for one ingest worker session.
///
/// Responsibilities:
/// - Execute protocol-specific ingest loop.
/// - Observe `stop_signal` and exit cooperatively.
/// - Report stream-level runtime outcomes via `WorkerLifecycle`.
///
/// Out of scope:
/// - No direct mutation of manager actor state.
/// - No global resource bookkeeping.
pub trait StreamAdapter: Send {
    async fn run(&mut self, ctx: &mut WorkerContext) -> anyhow::Result<()>;
}

/// Session runner that binds one concrete adapter with one worker context.
///
/// Responsibilities:
/// - Drive one adapter run with a normalized lifecycle envelope.
/// - Guarantee worker start/finalize notifications are emitted consistently.
///
/// Out of scope:
/// - No protocol branching logic.
/// - No global stream registry mutations.
pub struct IngestSession<T: StreamAdapter> {
    pub ctx: WorkerContext,
    pub adapter: T,
}

impl<T: StreamAdapter> IngestSession<T> {
    pub fn new(ctx: WorkerContext, adapter: T) -> Self {
        Self { ctx, adapter }
    }

    pub async fn start(mut self) {
        self.ctx.lifecycle.notify_ingest_worker_started();

        if let Err(e) = self.adapter.run(&mut self.ctx).await {
            self.ctx
                .lifecycle
                .finalize(&self.ctx.flv_packet_tx, Some(e.to_string()));
        } else {
            self.ctx.lifecycle.finalize(&self.ctx.flv_packet_tx, None);
        }
    }
}
