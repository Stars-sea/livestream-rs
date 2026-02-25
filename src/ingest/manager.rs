use anyhow::Result;
use log::{info, warn};
use tokio::fs;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReadDirStream;

use super::events::{StreamControlMessage, StreamMessage};
use super::handlers;
use super::port_allocator::PortAllocator;
use super::pull_stream::pull_srt_loop;
use super::settings::Settings;
use super::stream_info::StreamInfo;

use crate::core::output::FlvPacket;
use crate::services::MemoryCache;
use crate::services::MinioClient;

// Channel buffer sizes
const CHANNEL_SIZE: usize = 16;

#[derive(Debug)]
pub struct StreamManager {
    settings: Settings,
    stream_info_cache: MemoryCache<StreamInfo>,
    port_allocator: PortAllocator,
    control_tx: broadcast::Sender<StreamControlMessage>,
    stream_msg_tx: broadcast::Sender<StreamMessage>,
    flv_packet_tx: mpsc::Sender<FlvPacket>,
}

impl StreamManager {
    pub fn new(minio_client: MinioClient, flv_packet_tx: mpsc::Sender<FlvPacket>) -> Self {
        let settings = Settings::load().expect("Failed to load settings");

        let (control_tx, _) = broadcast::channel::<StreamControlMessage>(CHANNEL_SIZE);
        let (stream_msg_tx, stream_msg_rx) = broadcast::channel::<StreamMessage>(CHANNEL_SIZE);

        tokio::spawn(handlers::stream_message_handler(
            stream_msg_rx,
            settings.grpc_callback.clone(),
            minio_client,
        ));

        let port_allocator = {
            let (start_port, end_port) = settings
                .srt_port_range()
                .expect("Invalid SRT port range in settings");
            PortAllocator::new(start_port, end_port)
        };

        Self {
            settings,
            stream_info_cache: MemoryCache::new(),
            port_allocator,
            control_tx,
            stream_msg_tx,
            flv_packet_tx,
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

        info!(
            "Ready to pull stream at srt port: {} (LiveId: {live_id})",
            stream_info.srt_port()
        );

        let result = pull_srt_loop(
            self.stream_msg_tx.clone(),
            self.control_tx.clone(),
            self.flv_packet_tx.clone(),
            stream_info.clone(),
        )
        .await;

        self.stream_info_cache.remove(&live_id).await;

        if let Err(e) = self.release_stream_resources(stream_info).await {
            warn!("Failed to release resources for stream {live_id}: {e}");
        }

        result
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
