use std::sync::Arc;

use anyhow::Result;
use log::{error, info, warn};
use tokio::fs;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReadDirStream;

use super::events::{StreamControlMessage, StreamMessage};
use super::handlers;
use super::port_allocator::PortAllocator;
use super::puller::StreamPullerFactory;
use super::stream_info::StreamInfo;

use crate::Settings;
use crate::core::output::FlvPacket;
use crate::services::MemoryCache;
use crate::services::MinioClient;

#[derive(Debug)]
pub struct StreamManager {
    settings: Settings,

    puller_factory: Arc<StreamPullerFactory>,
    stream_info_cache: MemoryCache<StreamInfo>,
    port_allocator: PortAllocator,

    control_tx: mpsc::UnboundedSender<StreamControlMessage>,
}

impl StreamManager {
    pub fn new(
        settings: Settings,
        minio_client: MinioClient,
        flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
    ) -> Self {
        let (stream_msg_tx, stream_msg_rx) = mpsc::unbounded_channel::<StreamMessage>();
        let puller_factory = Arc::new(StreamPullerFactory::new(stream_msg_tx, flv_packet_tx));

        let (control_tx, control_rx) = mpsc::unbounded_channel::<StreamControlMessage>();
        tokio::spawn(handlers::stream_control_message_handler(
            control_rx,
            puller_factory.clone(),
        ));

        tokio::spawn(handlers::stream_message_handler(
            stream_msg_rx,
            settings.grpc_callback.clone(),
            minio_client,
            puller_factory.clone(),
        ));

        let port_allocator = {
            let (start_port, end_port) = settings
                .srt_port_range()
                .expect("Invalid SRT port range in settings");
            PortAllocator::new(start_port, end_port)
        };

        Self {
            settings,
            puller_factory,
            stream_info_cache: MemoryCache::new(),
            port_allocator,
            control_tx,
        }
    }

    pub async fn make_stream_info(&self, live_id: &str, passphrase: &str) -> Result<StreamInfo> {
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

        if let Err(e) = fs::create_dir_all(info.cache_dir()).await {
            self.port_allocator.release_port(port).await;
            return Err(anyhow::anyhow!("Failed to create cache directory: {e}"));
        }

        Ok(info)
    }

    pub async fn release_stream_resources(&self, info: StreamInfo) -> Result<()> {
        // Release allocated port
        self.port_allocator.release_port(info.srt_port()).await;

        // Remove empty cache directory
        let read_dir = fs::read_dir(info.cache_dir()).await?;
        let entries_stream = ReadDirStream::new(read_dir);

        let is_not_empty = entries_stream
            .filter_map(|entry| entry.ok())
            .any(|entry| {
                let filename = entry.file_name();
                filename != "." && filename != ".."
            })
            .await;

        if !is_not_empty {
            if let Err(e) = fs::remove_dir_all(info.cache_dir()).await {
                anyhow::bail!("Failed to remove cache directory: {e}");
            }
        }

        Ok(())
    }

    pub async fn start_stream(&self, stream_info: StreamInfo) -> Result<()> {
        let live_id = stream_info.live_id().to_string();

        self.stream_info_cache
            .set(live_id.clone(), stream_info.clone())
            .await?;

        let cloned_info = stream_info.clone();
        let factory = self.puller_factory.clone();
        if !factory.can_create(&stream_info).await {
            warn!("Cannot create stream puller for live_id: {}", live_id);
            anyhow::bail!("Failed to create stream puller for live_id: {live_id}");
        }

        info!(
            "Ready to pull stream at srt port: {} (LiveId: {live_id})",
            stream_info.srt_port()
        );

        tokio::task::spawn_blocking(move || {
            let puller = factory.create_blocking(cloned_info);
            if let Err(e) = puller {
                error!("Failed to create stream puller: {e}");
                return;
            }

            if let Err(e) = puller.unwrap().start() {
                error!("Stream puller error: {e}");
            }
        });

        self.stream_info_cache.remove(&live_id).await;

        if let Err(e) = self.release_stream_resources(stream_info).await {
            warn!("Failed to release resources for stream {live_id}: {e}");
        }

        Ok(())
    }

    pub async fn stop_stream(&self, live_id: &str) -> Result<()> {
        self.control_tx
            .send(StreamControlMessage::stop_stream(live_id))?;
        Ok(())
    }

    pub async fn list_active_streams(&self) -> Result<Vec<String>> {
        Ok(self.stream_info_cache.keys().await)
    }

    pub async fn get_stream_info(&self, live_id: &str) -> Option<StreamInfo> {
        match self.stream_info_cache.get(live_id).await {
            Some(info) => Some(info),
            None => {
                warn!("Failed to get stream info for live_id: {live_id}");
                None
            }
        }
    }
}
