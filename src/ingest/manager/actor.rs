use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use crossfire::{AsyncRx, MAsyncTx, MTx, mpsc, oneshot};
use tokio::time::error::Elapsed;
use tokio::time::timeout;
use tracing::Span;

use super::command::ManagerCommand;
use super::context::StreamContext;
use super::state::ManagerStreamState;
use crate::config::{EgressConfig, IngestConfig};
use crate::ingest::adapters::{RtmpAdapter, SrtAdapter};
use crate::ingest::events::StreamMessage;
use crate::ingest::session;
use crate::ingest::{PortAllocator, StreamInfo};
use crate::media::output::FlvPacket;
use crate::telemetry::metrics;

/// Single-threaded authority for stream state and resource coordination.
///
/// Responsibilities:
/// - Own and mutate global stream registry (`stream_contexts`).
/// - Allocate/release ports and coordinate stream start/stop/remove transitions.
/// - Execute `ManagerCommand`s sequentially to avoid shared-state races.
///
/// Out of scope:
/// - No media packet processing (done by adapters/sessions).
/// - No external callback business logic (handled by event handlers).
pub(super) struct ManagerActor {
    rx: AsyncRx<mpsc::Array<ManagerCommand>>,
    stream_contexts: HashMap<String, StreamContext>,
    port_allocator: Arc<PortAllocator>,
    ingest_host: String,
    segment_duration: i32,
    egress_port: u16,
    egress_appname: String,
    flv_packet_tx: MTx<mpsc::List<FlvPacket>>,
    stream_msg_tx: MTx<mpsc::List<(StreamMessage, Span)>>,
    self_cmd_tx: MAsyncTx<mpsc::Array<ManagerCommand>>,
}

impl ManagerActor {
    pub fn new(
        rx: AsyncRx<mpsc::Array<ManagerCommand>>,
        flv_packet_tx: MTx<mpsc::List<FlvPacket>>,
        stream_msg_tx: MTx<mpsc::List<(StreamMessage, Span)>>,
        self_cmd_tx: MAsyncTx<mpsc::Array<ManagerCommand>>,
        port_allocator: Arc<PortAllocator>,
        ingest_config: IngestConfig,
        egress_config: EgressConfig,
    ) -> Self {
        Self {
            rx,
            stream_contexts: HashMap::new(),
            port_allocator,
            ingest_host: ingest_config.host,
            segment_duration: ingest_config.duration,
            egress_port: egress_config.port,
            egress_appname: egress_config.appname,
            flv_packet_tx,
            stream_msg_tx,
            self_cmd_tx,
        }
    }

    async fn handle_remove_stream(&mut self, live_id: &str) {
        if let Some(ctx) = self.stream_contexts.remove(live_id) {
            if let Some(srt_options) = ctx.info.srt_options() {
                self.port_allocator.release_port(srt_options.port()).await;
            }
        }
    }

    pub async fn run(mut self) {
        while let Ok(cmd) = self.rx.recv().await {
            self.handle_command(cmd).await;
        }
    }

    async fn handle_command(&mut self, cmd: ManagerCommand) {
        match cmd {
            ManagerCommand::MakeSrtStreamInfo {
                live_id,
                passphrase,
                reply,
            } => {
                self.handle_make_srt_stream_info(live_id, passphrase, reply)
                    .await;
            }
            ManagerCommand::MakeRtmpStreamInfo { live_id, reply } => {
                self.handle_make_rtmp_stream_info(live_id, reply);
            }
            ManagerCommand::StartSrtStream { info, reply } => {
                self.handle_start_srt_stream(info, reply);
            }
            ManagerCommand::StartRtmpStream { info, reply } => {
                self.handle_start_rtmp_stream(info, reply);
            }
            ManagerCommand::StopStream { live_id, reply } => {
                self.handle_stop_stream(live_id, reply).await;
            }
            ManagerCommand::RemoveStream { live_id, reply } => {
                self.handle_remove_stream(&live_id).await;
                let _ = reply.send(());
            }
            ManagerCommand::Shutdown { reply } => {
                self.handle_shutdown(reply).await;
            }
            ManagerCommand::ListActiveStreams { reply } => {
                let keys = self.stream_contexts.keys().cloned().collect();
                let _ = reply.send(Ok(keys));
            }
            ManagerCommand::IsStreamsEmpty { reply } => {
                let _ = reply.send(self.stream_contexts.is_empty());
            }
            ManagerCommand::HasStream { live_id, reply } => {
                let _ = reply.send(self.stream_contexts.contains_key(&live_id));
            }
            ManagerCommand::GetStreamInfo { live_id, reply } => {
                let info = self.stream_contexts.get(&live_id).map(|c| c.info.clone());
                let _ = reply.send(info);
            }
            ManagerCommand::GetRtmpTx { live_id, reply } => {
                let tx = self
                    .stream_contexts
                    .get(&live_id)
                    .and_then(|c| c.rtmp_tx.clone());
                let _ = reply.send(tx);
            }
        }
    }

    async fn handle_make_srt_stream_info(
        &mut self,
        live_id: String,
        passphrase: String,
        reply: oneshot::TxOneshot<Result<Arc<StreamInfo>>>,
    ) {
        if self.stream_contexts.contains_key(&live_id) {
            let _ = reply.send(Err(anyhow::anyhow!("Stream '{}' already exists", live_id)));
            return;
        }

        if let Some(port) = self.port_allocator.allocate_safe_port().await {
            match StreamInfo::new_srt(
                live_id.clone(),
                self.ingest_host.clone(),
                self.segment_duration,
                port,
                passphrase,
            ) {
                Ok(info) => {
                    let info = Arc::new(info);
                    let ctx = StreamContext {
                        info: info.clone(),
                        stop_signal: Arc::new(AtomicBool::new(false)),
                        rtmp_tx: None,
                        state: ManagerStreamState::Created,
                    };
                    self.stream_contexts.insert(live_id, ctx);
                    let _ = reply.send(Ok(info));
                }
                Err(e) => {
                    self.port_allocator.release_port(port).await;
                    let _ = reply.send(Err(anyhow::anyhow!("Failed to create stream info: {}", e)));
                }
            }
        } else {
            let _ = reply.send(Err(anyhow::anyhow!(
                "No available ports to allocate for SRT stream"
            )));
        }
    }

    fn handle_make_rtmp_stream_info(
        &mut self,
        live_id: String,
        reply: oneshot::TxOneshot<Result<Arc<StreamInfo>>>,
    ) {
        if self.stream_contexts.contains_key(&live_id) {
            let _ = reply.send(Err(anyhow::anyhow!("Stream '{}' already exists", live_id)));
            return;
        }

        match StreamInfo::new_rtmp(
            live_id.clone(),
            self.ingest_host.clone(),
            self.egress_port,
            self.egress_appname.clone(),
            self.segment_duration,
        ) {
            Ok(info) => {
                let info = Arc::new(info);
                let ctx = StreamContext {
                    info: info.clone(),
                    stop_signal: Arc::new(AtomicBool::new(false)),
                    rtmp_tx: None,
                    state: ManagerStreamState::Created,
                };
                self.stream_contexts.insert(live_id, ctx);
                let _ = reply.send(Ok(info));
            }
            Err(e) => {
                let _ = reply.send(Err(anyhow::anyhow!("Failed to create stream info: {}", e)));
            }
        }
    }

    fn handle_start_srt_stream(
        &mut self,
        info: Arc<StreamInfo>,
        reply: oneshot::TxOneshot<Result<()>>,
    ) {
        let live_id = info.live_id().to_string();
        if info.srt_options().is_none() {
            let _ = reply.send(Err(anyhow::anyhow!(
                "RTMP ingest stream '{}' is handled by server RTMP worker, not FFmpeg SRT worker",
                live_id
            )));
            return;
        }

        let Some(existing_ctx) = self.stream_contexts.get(&live_id) else {
            let _ = reply.send(Err(anyhow::anyhow!("Stream '{}' not found", live_id)));
            return;
        };

        if matches!(
            existing_ctx.state,
            ManagerStreamState::Starting
                | ManagerStreamState::Running
                | ManagerStreamState::Stopping
        ) {
            let _ = reply.send(Err(anyhow::anyhow!(
                "Stream '{}' is already active",
                live_id
            )));
            return;
        }

        let stop_signal = self
            .stream_contexts
            .get(&live_id)
            .map(|ctx| ctx.stop_signal.clone())
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));

        if let Some(ctx) = self.stream_contexts.get_mut(&live_id) {
            ctx.state = ManagerStreamState::Starting;
        }

        let ctx = session::WorkerContext::new(
            info.clone(),
            stop_signal,
            self.stream_msg_tx.clone(),
            self.flv_packet_tx.clone(),
        );
        let adapter = SrtAdapter::new();
        let session = session::IngestSession::new(ctx, adapter);

        let self_cmd_tx = self.self_cmd_tx.clone();
        let cloned_info = info.clone();
        let parent_span = Span::current();

        tokio::spawn(async move {
            let _entered = parent_span.enter();
            let _stream_guard =
                metrics::MetricGuard::new(&metrics::get_metrics().ingest_streams, vec![]);

            session.start().await;

            Self::wait_for_cache_dir_empty(&cloned_info).await.ok();

            let _ = self_cmd_tx
                .send(ManagerCommand::RemoveStream {
                    live_id: cloned_info.live_id().to_string(),
                    reply: oneshot::oneshot().0,
                })
                .await;
        });

        if let Some(ctx) = self.stream_contexts.get_mut(&live_id) {
            ctx.state = ManagerStreamState::Running;
        }

        let _ = reply.send(Ok(()));
    }

    fn handle_start_rtmp_stream(
        &mut self,
        info: Arc<StreamInfo>,
        reply: oneshot::TxOneshot<Result<()>>,
    ) {
        let live_id = info.live_id().to_string();

        let Some(existing_ctx) = self.stream_contexts.get(&live_id) else {
            let _ = reply.send(Err(anyhow::anyhow!("Stream '{}' not found", live_id)));
            return;
        };

        if matches!(
            existing_ctx.state,
            ManagerStreamState::Starting
                | ManagerStreamState::Running
                | ManagerStreamState::Stopping
        ) {
            let _ = reply.send(Err(anyhow::anyhow!(
                "Stream '{}' is already active",
                live_id
            )));
            return;
        }

        let stop_signal = self
            .stream_contexts
            .get(&live_id)
            .map(|ctx| ctx.stop_signal.clone())
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));

        let (rtmp_tx, rtmp_rx) = mpsc::unbounded_async();
        if let Some(ctx) = self.stream_contexts.get_mut(&live_id) {
            ctx.rtmp_tx = Some(rtmp_tx);
            ctx.state = ManagerStreamState::Starting;
        }

        let ctx = session::WorkerContext::new(
            info.clone(),
            stop_signal,
            self.stream_msg_tx.clone(),
            self.flv_packet_tx.clone(),
        );
        let adapter = RtmpAdapter::new(rtmp_rx);
        let session = session::IngestSession::new(ctx, adapter);

        let self_cmd_tx = self.self_cmd_tx.clone();
        let cloned_info = info.clone();

        tokio::spawn(async move {
            let _stream_guard =
                metrics::MetricGuard::new(&metrics::get_metrics().ingest_streams, vec![]);

            session.start().await;

            Self::wait_for_cache_dir_empty(&cloned_info).await.ok();

            let _ = self_cmd_tx
                .send(ManagerCommand::RemoveStream {
                    live_id: cloned_info.live_id().to_string(),
                    reply: oneshot::oneshot().0,
                })
                .await;
        });

        if let Some(ctx) = self.stream_contexts.get_mut(&live_id) {
            ctx.state = ManagerStreamState::Running;
        }

        let _ = reply.send(Ok(()));
    }

    async fn handle_stop_stream(&mut self, live_id: String, reply: oneshot::TxOneshot<Result<()>>) {
        let Some(ctx) = self.stream_contexts.get(&live_id) else {
            let _ = reply.send(Err(anyhow::anyhow!("Stream '{}' not found", live_id)));
            return;
        };

        if matches!(
            ctx.state,
            ManagerStreamState::Stopping | ManagerStreamState::Stopped
        ) {
            let _ = reply.send(Ok(()));
            return;
        }

        ctx.stop_signal.store(true, Ordering::SeqCst);

        let state = ctx.state;

        if matches!(
            state,
            ManagerStreamState::Starting | ManagerStreamState::Running
        ) && let Some(ctx_mut) = self.stream_contexts.get_mut(&live_id)
        {
            ctx_mut.state = ManagerStreamState::Stopping;
        }

        // Keep started streams in the registry until workers exit, so port release
        // and cleanup happen after actual shutdown via RemoveStream.
        if matches!(state, ManagerStreamState::Created) {
            if let Some(ctx_mut) = self.stream_contexts.get_mut(&live_id) {
                ctx_mut.state = ManagerStreamState::Stopped;
            }
            self.handle_remove_stream(&live_id).await;
            let _ = self.flv_packet_tx.send(FlvPacket::EndOfStream { live_id });
        }

        let _ = reply.send(Ok(()));
    }

    async fn handle_shutdown(&mut self, reply: oneshot::TxOneshot<()>) {
        for ctx in self.stream_contexts.values() {
            ctx.stop_signal.store(true, Ordering::SeqCst);
        }
        let keys: Vec<String> = self.stream_contexts.keys().cloned().collect();
        for key in keys {
            self.handle_remove_stream(&key).await;
        }
        let _ = reply.send(());
    }

    async fn wait_for_cache_dir_empty(info: &Arc<StreamInfo>) -> std::result::Result<(), Elapsed> {
        timeout(Duration::from_secs(3), async {
            while !info.is_cache_empty().unwrap_or(true) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await?;
        Ok(())
    }
}
