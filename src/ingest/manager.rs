use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::{future::Future, pin::Pin};

use anyhow::Result;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{Span, error, info, instrument};

use super::handlers;
use super::port_allocator::PortAllocator;
use super::rtmp_worker::{RtmpTag, RtmpWorker};
use super::srt_worker::SrtWorker;
use super::stream_info::StreamInfo;

use crate::core::output::FlvPacket;
use crate::otlp::metrics;
use crate::server::contracts::StreamRegistry;
use crate::services::GrpcClientFactory;
use crate::services::MinioClient;
use dashmap::DashMap;

#[derive(Debug)]
pub struct StreamContext {
    pub info: Arc<StreamInfo>,
    pub stop_signal: Arc<std::sync::atomic::AtomicBool>,
    pub rtmp_tx: Option<mpsc::UnboundedSender<RtmpTag>>,
}

#[derive(Debug)]
pub struct StreamManager {
    stream_contexts: Arc<DashMap<String, StreamContext>>,
    port_allocator: Arc<PortAllocator>,
    flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
    stream_msg_tx: mpsc::UnboundedSender<(super::events::StreamMessage, Span)>,
}

impl StreamManager {
    pub fn new(
        minio_client: MinioClient,
        flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
        stream_msg_tx: mpsc::UnboundedSender<(super::events::StreamMessage, Span)>,
        stream_msg_rx: mpsc::UnboundedReceiver<(super::events::StreamMessage, Span)>,
    ) -> Self {
        let stream_contexts = Arc::new(DashMap::new());
        let port_allocator = Arc::new(PortAllocator::default());

        tokio::spawn(handlers::stream_message_handler(
            stream_msg_rx,
            GrpcClientFactory::default(),
            minio_client,
            stream_contexts.clone(),
            port_allocator.clone(),
        ));

        Self {
            stream_contexts,
            port_allocator,
            flv_packet_tx,
            stream_msg_tx,
        }
    }

    pub async fn make_srt_stream_info(
        &self,
        live_id: &str,
        passphrase: &str,
    ) -> Result<Arc<StreamInfo>> {
        let port = self
            .port_allocator
            .allocate_safe_port()
            .await
            .ok_or(anyhow::anyhow!(
                "No available ports to allocate for SRT stream"
            ))?;

        let info = StreamInfo::new_srt(live_id.to_string(), port, passphrase.to_string());

        if let Err(e) = info {
            self.port_allocator.release_port(port).await;
            anyhow::bail!("Failed to create stream info: {e}");
        }

        let info = Arc::new(info?);
        let ctx = StreamContext {
            info: info.clone(),
            stop_signal: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rtmp_tx: None,
        };
        self.stream_contexts.insert(live_id.to_string(), ctx);

        Ok(info)
    }

    pub async fn make_rtmp_stream_info(&self, live_id: &str) -> Result<Arc<StreamInfo>> {
        let info = Arc::new(StreamInfo::new_rtmp(live_id.to_string())?);
        let ctx = StreamContext {
            info: info.clone(),
            stop_signal: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rtmp_tx: None,
        };
        self.stream_contexts.insert(live_id.to_string(), ctx);
        Ok(info)
    }

    async fn release_stream_resources(&self, info: Arc<StreamInfo>) {
        if let Some(srt_options) = info.srt_options() {
            self.port_allocator.release_port(srt_options.port()).await;
        }

        self.stream_contexts.remove(info.live_id());
    }

    async fn release_stream_by_id(&self, live_id: &str) -> bool {
        if let Some(info) = self.get_stream_info(live_id).await {
            self.release_stream_resources(info).await;
            return true;
        }
        false
    }

    #[instrument(name = "ingest.manager.start", skip(self, stream_info), fields(stream.live_id = %stream_info.live_id()))]
    pub async fn start_srt_stream(self: &Arc<Self>, stream_info: Arc<StreamInfo>) -> Result<()> {
        let live_id = stream_info.live_id().to_string();

        if stream_info.srt_options().is_none() {
            anyhow::bail!(
                "RTMP ingest stream '{}' is handled by server RTMP worker, not FFmpeg SRT worker",
                live_id
            );
        }

        if let Some(options) = stream_info.srt_options() {
            info!(
                live_id = %live_id,
                port = options.port(),
                "Starting stream process with SRT input"
            );
        }

        let cloned_info = stream_info.clone();
        let arc_self = self.clone();
        let parent_span = Span::current();

        let stop_signal = self
            .stream_contexts
            .get(&live_id)
            .map(|ctx| ctx.stop_signal.clone())
            .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicBool::new(false)));

        let stream_msg_tx = self.stream_msg_tx.clone();
        let flv_packet_tx = self.flv_packet_tx.clone();

        tokio::task::spawn_blocking(move || {
            let _entered = parent_span.enter();
            let handle = tokio::runtime::Handle::current();

            let _stream_guard =
                metrics::MetricGuard::new(&metrics::get_metrics().ingest_streams, vec![]);

            let mut worker = SrtWorker::new(
                cloned_info.clone(),
                stream_msg_tx,
                flv_packet_tx,
                stop_signal,
            );

            worker.start();

            handle.block_on(async {
                timeout(Duration::from_secs(3), async {
                    while !cloned_info.is_cache_empty().unwrap_or(true) {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                })
                .await
                .ok();
                arc_self.release_stream_resources(cloned_info).await
            });
        });

        Ok(())
    }

    #[instrument(name = "ingest.manager.start_rtmp", skip(self, stream_info), fields(stream.live_id = %stream_info.live_id()))]
    pub async fn start_rtmp_stream(self: &Arc<Self>, stream_info: Arc<StreamInfo>) -> Result<()> {
        let live_id = stream_info.live_id().to_string();

        let stop_signal = self
            .stream_contexts
            .get(&live_id)
            .map(|ctx| ctx.stop_signal.clone())
            .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicBool::new(false)));

        let (rtmp_tx, rtmp_rx) = mpsc::unbounded_channel();

        if let Some(mut ctx) = self.stream_contexts.get_mut(&live_id) {
            ctx.rtmp_tx = Some(rtmp_tx);
        }

        let worker = RtmpWorker::new(
            stream_info.clone(),
            self.stream_msg_tx.clone(),
            self.flv_packet_tx.clone(),
            stop_signal,
            rtmp_rx,
        );

        let cloned_info = stream_info.clone();
        let arc_self = self.clone();

        tokio::spawn(async move {
            let _stream_guard =
                metrics::MetricGuard::new(&metrics::get_metrics().ingest_streams, vec![]);

            worker.run().await;

            timeout(Duration::from_secs(3), async {
                while !cloned_info.is_cache_empty().unwrap_or(true) {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await
            .ok();
            arc_self.release_stream_resources(cloned_info).await;
        });

        Ok(())
    }

    #[instrument(name = "ingest.manager.stop", skip(self), fields(stream.live_id = %live_id))]
    pub async fn stop_stream(&self, live_id: &str) -> Result<()> {
        info!(live_id = %live_id, "Stopping stream request");
        if let Some(ctx) = self.stream_contexts.get(live_id) {
            ctx.stop_signal.store(true, Ordering::SeqCst);
        }

        if self.release_stream_by_id(live_id).await {
            let _ = self.flv_packet_tx.send(FlvPacket::EndOfStream {
                live_id: live_id.to_string(),
            });
            return Ok(());
        }

        error!(live_id = %live_id, "No active stream found to stop");
        anyhow::bail!("No active stream found for live_id: {}", live_id);
    }

    #[instrument(name = "ingest.manager.shutdown", skip(self))]
    pub async fn shutdown(&self) {
        for entry in self.stream_contexts.iter() {
            entry.value().stop_signal.store(true, Ordering::SeqCst);
        }
        let keys: Vec<String> = self
            .stream_contexts
            .iter()
            .map(|e| e.key().clone())
            .collect();
        for key in keys {
            let _ = self.release_stream_by_id(&key).await;
        }
    }

    #[instrument(name = "ingest.manager.list_active", skip(self))]
    pub async fn list_active_streams(&self) -> Result<Vec<String>> {
        Ok(self
            .stream_contexts
            .iter()
            .map(|entry| entry.key().clone())
            .collect())
    }

    #[instrument(name = "ingest.manager.is_empty", skip(self))]
    pub async fn is_streams_empty(&self) -> bool {
        self.stream_contexts.is_empty()
    }

    #[allow(dead_code)]
    #[instrument(name = "ingest.manager.exists", skip(self), fields(stream.live_id = %live_id))]
    pub async fn has_stream(&self, live_id: &str) -> bool {
        self.stream_contexts.contains_key(live_id)
    }

    #[instrument(name = "ingest.manager.get_info", skip(self), fields(stream.live_id = %live_id))]
    pub async fn get_stream_info(&self, live_id: &str) -> Option<Arc<StreamInfo>> {
        self.stream_contexts
            .get(live_id)
            .map(|ctx| ctx.value().info.clone())
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
            self.stream_contexts
                .get(live_id)
                .and_then(|ctx| ctx.value().rtmp_tx.clone())
        })
    }
}
