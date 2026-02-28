use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{Span, error, info, instrument, warn};

use super::factory::GrpcClientFactory;
use super::handlers;
use super::port_allocator::PortAllocator;
use super::puller::StreamPullerFactory;
use super::stream_info::StreamInfo;

use crate::core::output::FlvPacket;
use crate::services::MemoryCache;
use crate::services::MinioClient;
use crate::settings::IngestConfig;

#[derive(Debug)]
pub struct StreamManager {
    settings: IngestConfig,

    puller_factory: Arc<StreamPullerFactory>,
    stream_info_cache: MemoryCache<Arc<StreamInfo>>,
    port_allocator: PortAllocator,
}

impl StreamManager {
    pub fn new(
        config: IngestConfig,
        minio_client: MinioClient,
        flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
    ) -> Self {
        let (stream_msg_tx, stream_msg_rx) = mpsc::unbounded_channel();
        let puller_factory = Arc::new(StreamPullerFactory::new(stream_msg_tx, flv_packet_tx));

        tokio::spawn(handlers::stream_message_handler(
            stream_msg_rx,
            GrpcClientFactory::new(config.callback.clone()),
            minio_client,
            puller_factory.clone(),
        ));

        let port_allocator = {
            let (start_port, end_port) = config
                .srt_port_range()
                .expect("Invalid SRT port range in settings");
            PortAllocator::new(start_port, end_port)
        };

        Self {
            settings: config,
            puller_factory,
            stream_info_cache: MemoryCache::new(),
            port_allocator,
        }
    }

    pub async fn make_stream_info(
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

        let info = StreamInfo::new(
            live_id.to_string(),
            port,
            passphrase.to_string(),
            &self.settings,
        );

        if let Err(e) = info {
            self.port_allocator.release_port(port).await;
            anyhow::bail!("Failed to create stream info: {e}");
        }

        let info = Arc::new(info?);
        self.stream_info_cache
            .set(live_id.to_string(), info.clone())
            .await?;

        Ok(info)
    }

    async fn release_stream_resources(&self, info: Arc<StreamInfo>) {
        self.port_allocator.release_port(info.srt_port()).await;

        self.stream_info_cache.remove(info.live_id()).await;
    }

    #[instrument(name = "ingest.stream.start", skip(self, stream_info), fields(stream.live_id = %stream_info.live_id(), stream.srt_port = stream_info.srt_port()))]
    pub async fn start_stream(self: &Arc<Self>, stream_info: Arc<StreamInfo>) -> Result<()> {
        let live_id = stream_info.live_id().to_string();

        if !self.puller_factory.can_create(&stream_info).await {
            warn!(live_id = %live_id, "Cannot create stream puller");
            anyhow::bail!("Failed to create stream puller for live_id: {live_id}");
        }

        info!(live_id = %live_id, port = stream_info.srt_port(), "Starting stream process");

        let cloned_info = stream_info.clone();
        let arc_self = self.clone();
        let parent_span = Span::current();
        tokio::task::spawn_blocking(move || {
            let _entered = parent_span.enter();
            let handle = tokio::runtime::Handle::current();

            match handle.block_on(arc_self.puller_factory.create(cloned_info.clone())) {
                Ok(mut puller) => {
                    if let Err(e) = puller.start() {
                        error!(error = %e, "Stream puller error");
                    }
                }
                Err(e) => {
                    error!(error = %e, "Failed to create stream puller");
                }
            }

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

    #[instrument(name = "ingest.stream.stop", skip(self), fields(stream.live_id = %live_id))]
    pub async fn stop_stream(&self, live_id: &str) -> Result<()> {
        info!(live_id = %live_id, "Stopping stream request");
        if let Some(signal) = self.puller_factory.get_signal(live_id).await {
            signal.store(true, Ordering::SeqCst);
        } else {
            error!(live_id = %live_id, "No active stream found to stop");
            anyhow::bail!("No active stream found for live_id: {}", live_id);
        }
        Ok(())
    }

    #[instrument(name = "ingest.stream.shutdown", skip(self))]
    pub async fn shutdown(&self) {
        for signal in self.puller_factory.get_signals().await {
            signal.store(true, Ordering::SeqCst);
        }
    }

    #[instrument(name = "ingest.stream.list_active", skip(self))]
    pub async fn list_active_streams(&self) -> Result<Vec<String>> {
        Ok(self.stream_info_cache.keys().await)
    }

    #[instrument(name = "ingest.stream.is_empty", skip(self))]
    pub async fn is_streams_empty(&self) -> bool {
        self.stream_info_cache.keys().await.is_empty()
    }

    #[instrument(name = "ingest.stream.exists", skip(self), fields(stream.live_id = %live_id))]
    pub async fn has_stream(&self, live_id: &str) -> bool {
        self.stream_info_cache.contains_key(live_id).await
    }

    #[instrument(name = "ingest.stream.get_info", skip(self), fields(stream.live_id = %live_id))]
    pub async fn get_stream_info(&self, live_id: &str) -> Option<Arc<StreamInfo>> {
        self.stream_info_cache.get(live_id).await
    }
}
