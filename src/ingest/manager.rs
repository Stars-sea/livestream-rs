use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{mpsc, oneshot};
use tokio::time::error::Elapsed;
use tokio::time::timeout;
use tracing::{Span, instrument};

use super::adapters::rtmp::{RtmpAdapter, RtmpTag};
use super::adapters::srt::SrtAdapter;
use super::events::{StreamMessage, handlers};
use super::port_allocator::PortAllocator;
use super::stream_info::StreamInfo;

use crate::api::grpc::contracts::StreamRegistry;
use crate::infra::GrpcClientFactory;
use crate::infra::MinioClient;
use crate::ingest::session;
use crate::media::output::FlvPacket;
use crate::telemetry::metrics;

/// Context for an active stream, holding its configuration, life-cycle controls,
/// and protocol-specific communication channels.
#[derive(Debug)]
pub struct StreamContext {
    /// Information and configuration (e.g., live ID, ports) of the current stream.
    pub info: Arc<StreamInfo>,
    /// Signal used to indicate that the stream should be stopped.
    pub stop_signal: Arc<AtomicBool>,
    /// Optional transmitter for routing RTMP media tags, if the stream uses the RTMP protocol.
    pub rtmp_tx: Option<mpsc::UnboundedSender<RtmpTag>>,
    /// Indicates whether a stream worker has been started for this context.
    pub started: bool,
}

/// Internal command protocol between `StreamManager` and `ManagerActor`.
///
/// Responsibilities:
/// - Represent all state-changing and query operations for stream management.
/// - Carry a typed reply channel for request/response style coordination.
///
/// Out of scope:
/// - No business logic, validation, or side effects.
enum ManagerCommand {
    MakeSrtStreamInfo {
        live_id: String,
        passphrase: String,
        reply: oneshot::Sender<Result<Arc<StreamInfo>>>,
    },
    MakeRtmpStreamInfo {
        live_id: String,
        reply: oneshot::Sender<Result<Arc<StreamInfo>>>,
    },
    StartSrtStream {
        info: Arc<StreamInfo>,
        reply: oneshot::Sender<Result<()>>,
    },
    StartRtmpStream {
        info: Arc<StreamInfo>,
        reply: oneshot::Sender<Result<()>>,
    },
    StopStream {
        live_id: String,
        reply: oneshot::Sender<Result<()>>,
    },
    RemoveStream {
        live_id: String,
        reply: oneshot::Sender<()>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
    ListActiveStreams {
        reply: oneshot::Sender<Result<Vec<String>>>,
    },
    IsStreamsEmpty {
        reply: oneshot::Sender<bool>,
    },
    HasStream {
        live_id: String,
        reply: oneshot::Sender<bool>,
    },
    GetStreamInfo {
        live_id: String,
        reply: oneshot::Sender<Option<Arc<StreamInfo>>>,
    },
    GetRtmpTx {
        live_id: String,
        reply: oneshot::Sender<Option<mpsc::UnboundedSender<RtmpTag>>>,
    },
}

/// Public facade for stream lifecycle operations.
///
/// Responsibilities:
/// - Expose async APIs for callers (make/start/stop/list/get).
/// - Translate each API call into `ManagerCommand` and await typed replies.
///
/// Out of scope:
/// - No direct ownership or mutation of stream state.
/// - No protocol-specific ingest logic.
#[derive(Clone, Debug)]
pub struct StreamManager {
    cmd_tx: mpsc::Sender<ManagerCommand>,
}

impl StreamManager {
    pub fn new(
        minio_client: MinioClient,
        grpc_client_factory: GrpcClientFactory,
        flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(128);
        let (stream_msg_tx, stream_msg_rx) = mpsc::unbounded_channel();

        let actor = ManagerActor::new(cmd_rx, flv_packet_tx, stream_msg_tx.clone(), cmd_tx.clone());
        tokio::spawn(actor.run());

        let manager = Self { cmd_tx };

        tokio::spawn(handlers::stream_message_handler(
            stream_msg_rx,
            grpc_client_factory,
            minio_client,
            manager.clone(),
        ));

        manager
    }

    pub async fn make_srt_stream_info(
        &self,
        live_id: &str,
        passphrase: &str,
    ) -> Result<Arc<StreamInfo>> {
        let (reply, rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::MakeSrtStreamInfo {
                live_id: live_id.to_string(),
                passphrase: passphrase.to_string(),
                reply,
            })
            .await;
        rx.await?
    }

    pub async fn make_rtmp_stream_info(&self, live_id: &str) -> Result<Arc<StreamInfo>> {
        let (reply, rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::MakeRtmpStreamInfo {
                live_id: live_id.to_string(),
                reply,
            })
            .await;
        rx.await?
    }

    #[instrument(name = "ingest.manager.start", skip(self, stream_info), fields(stream.live_id = %stream_info.live_id()))]
    pub async fn start_srt_stream(self: &Arc<Self>, stream_info: Arc<StreamInfo>) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::StartSrtStream {
                info: stream_info,
                reply,
            })
            .await;
        rx.await?
    }

    #[instrument(name = "ingest.manager.start_rtmp", skip(self, stream_info), fields(stream.live_id = %stream_info.live_id()))]
    pub async fn start_rtmp_stream(self: &Arc<Self>, stream_info: Arc<StreamInfo>) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::StartRtmpStream {
                info: stream_info,
                reply,
            })
            .await;
        rx.await?
    }

    #[instrument(name = "ingest.manager.stop", skip(self), fields(stream.live_id = %live_id))]
    pub async fn stop_stream(&self, live_id: &str) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::StopStream {
                live_id: live_id.to_string(),
                reply,
            })
            .await;
        rx.await?
    }

    pub async fn remove_stream(&self, live_id: &str) {
        let (reply, rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::RemoveStream {
                live_id: live_id.to_string(),
                reply,
            })
            .await;
        let _ = rx.await;
    }

    #[instrument(name = "ingest.manager.shutdown", skip(self))]
    pub async fn shutdown(&self) {
        let (reply, rx) = oneshot::channel();
        let _ = self.cmd_tx.send(ManagerCommand::Shutdown { reply }).await;
        let _ = rx.await;
    }

    #[instrument(name = "ingest.manager.list_active", skip(self))]
    pub async fn list_active_streams(&self) -> Result<Vec<String>> {
        let (reply, rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::ListActiveStreams { reply })
            .await;
        rx.await?
    }

    #[instrument(name = "ingest.manager.is_empty", skip(self))]
    pub async fn is_streams_empty(&self) -> bool {
        let (reply, rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::IsStreamsEmpty { reply })
            .await;
        rx.await.unwrap_or(true)
    }

    #[allow(dead_code)]
    #[instrument(name = "ingest.manager.exists", skip(self), fields(stream.live_id = %live_id))]
    pub async fn has_stream(&self, live_id: &str) -> bool {
        let (reply, rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::HasStream {
                live_id: live_id.to_string(),
                reply,
            })
            .await;
        rx.await.unwrap_or(false)
    }

    #[instrument(name = "ingest.manager.get_info", skip(self), fields(stream.live_id = %live_id))]
    pub async fn get_stream_info(&self, live_id: &str) -> Option<Arc<StreamInfo>> {
        let (reply, rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::GetStreamInfo {
                live_id: live_id.to_string(),
                reply,
            })
            .await;
        rx.await.unwrap_or(None)
    }
}

impl StreamRegistry for StreamManager {
    fn get_stream<'a>(
        &'a self,
        live_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<Arc<StreamInfo>>> + Send + 'a>> {
        Box::pin(async move { self.get_stream_info(live_id).await })
    }

    fn get_rtmp_tx<'a>(
        &'a self,
        live_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<mpsc::UnboundedSender<RtmpTag>>> + Send + 'a>> {
        Box::pin(async move {
            let (reply, rx) = oneshot::channel();
            let _ = self
                .cmd_tx
                .send(ManagerCommand::GetRtmpTx {
                    live_id: live_id.to_string(),
                    reply,
                })
                .await;
            rx.await.unwrap_or(None)
        })
    }
}

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
struct ManagerActor {
    rx: mpsc::Receiver<ManagerCommand>,
    stream_contexts: HashMap<String, StreamContext>,
    port_allocator: Arc<PortAllocator>,
    flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
    stream_msg_tx: mpsc::UnboundedSender<(StreamMessage, Span)>,
    self_cmd_tx: mpsc::Sender<ManagerCommand>,
}

impl ManagerActor {
    fn new(
        rx: mpsc::Receiver<ManagerCommand>,
        flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
        stream_msg_tx: mpsc::UnboundedSender<(StreamMessage, Span)>,
        self_cmd_tx: mpsc::Sender<ManagerCommand>,
    ) -> Self {
        Self {
            rx,
            stream_contexts: HashMap::new(),
            port_allocator: Arc::new(PortAllocator::default()),
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

    async fn run(mut self) {
        while let Some(cmd) = self.rx.recv().await {
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
        reply: oneshot::Sender<Result<Arc<StreamInfo>>>,
    ) {
        if self.stream_contexts.contains_key(&live_id) {
            let _ = reply.send(Err(anyhow::anyhow!("Stream '{}' already exists", live_id)));
            return;
        }

        if let Some(port) = self.port_allocator.allocate_safe_port().await {
            match StreamInfo::new_srt(live_id.clone(), port, passphrase) {
                Ok(info) => {
                    let info = Arc::new(info);
                    let ctx = StreamContext {
                        info: info.clone(),
                        stop_signal: Arc::new(AtomicBool::new(false)),
                        rtmp_tx: None,
                        started: false,
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
        reply: oneshot::Sender<Result<Arc<StreamInfo>>>,
    ) {
        if self.stream_contexts.contains_key(&live_id) {
            let _ = reply.send(Err(anyhow::anyhow!("Stream '{}' already exists", live_id)));
            return;
        }

        match StreamInfo::new_rtmp(live_id.clone()) {
            Ok(info) => {
                let info = Arc::new(info);
                let ctx = StreamContext {
                    info: info.clone(),
                    stop_signal: Arc::new(AtomicBool::new(false)),
                    rtmp_tx: None,
                    started: false,
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
        reply: oneshot::Sender<Result<()>>,
    ) {
        let live_id = info.live_id().to_string();
        if info.srt_options().is_none() {
            let _ = reply.send(Err(anyhow::anyhow!(
                "RTMP ingest stream '{}' is handled by server RTMP worker, not FFmpeg SRT worker",
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
            ctx.started = true;
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
                    reply: oneshot::channel().0,
                })
                .await;
        });

        let _ = reply.send(Ok(()));
    }

    fn handle_start_rtmp_stream(
        &mut self,
        info: Arc<StreamInfo>,
        reply: oneshot::Sender<Result<()>>,
    ) {
        let live_id = info.live_id().to_string();
        let stop_signal = self
            .stream_contexts
            .get(&live_id)
            .map(|ctx| ctx.stop_signal.clone())
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));

        let (rtmp_tx, rtmp_rx) = mpsc::unbounded_channel();
        if let Some(ctx) = self.stream_contexts.get_mut(&live_id) {
            ctx.rtmp_tx = Some(rtmp_tx);
            ctx.started = true;
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
                    reply: oneshot::channel().0,
                })
                .await;
        });

        let _ = reply.send(Ok(()));
    }

    async fn handle_stop_stream(&mut self, live_id: String, reply: oneshot::Sender<Result<()>>) {
        let Some(ctx) = self.stream_contexts.get(&live_id) else {
            let _ = reply.send(Err(anyhow::anyhow!("Stream '{}' not found", live_id)));
            return;
        };

        ctx.stop_signal.store(true, Ordering::SeqCst);

        // Keep started streams in the registry until workers exit, so port release
        // and cleanup happen after actual shutdown via RemoveStream.
        if !ctx.started {
            self.handle_remove_stream(&live_id).await;
            let _ = self.flv_packet_tx.send(FlvPacket::EndOfStream { live_id });
        }

        let _ = reply.send(Ok(()));
    }

    async fn handle_shutdown(&mut self, reply: oneshot::Sender<()>) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_manager_actor_flow() {
        let (cmd_tx, cmd_rx) = mpsc::channel(10);
        let (flv_tx, _flv_rx) = mpsc::unbounded_channel();
        let (msg_tx, _msg_rx) = mpsc::unbounded_channel();

        let actor = ManagerActor::new(cmd_rx, flv_tx, msg_tx, cmd_tx.clone());
        tokio::spawn(actor.run());

        let manager = StreamManager { cmd_tx };

        let streams = manager.list_active_streams().await.unwrap();
        assert!(streams.is_empty());
        assert!(manager.is_streams_empty().await);

        let info = manager.make_rtmp_stream_info("test_live").await.unwrap();
        assert_eq!(info.live_id(), "test_live");

        assert!(manager.has_stream("test_live").await);
        let active = manager.list_active_streams().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0], "test_live");

        manager.stop_stream("test_live").await.unwrap();
        assert!(!manager.has_stream("test_live").await);

        manager.shutdown().await;
    }
}
