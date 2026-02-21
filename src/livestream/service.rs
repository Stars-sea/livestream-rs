//! Live stream service managing SRT stream pulling and processing.

use super::events::*;
use super::grpc::livestream_server::Livestream;
use super::grpc::*;
use super::handlers;
use super::port_allocator::PortAllocator;
use super::pull_stream::pull_srt_loop;
use super::stream_info::StreamInfo;

use crate::services::MemoryCache;
use crate::services::MinioClient;
use crate::settings::Settings;

use std::sync::Arc;

use anyhow::Result;
use log::{info, warn};
use tokio::fs;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReadDirStream;
use tonic::{Request, Response, Status};

pub use super::grpc::livestream_server::LivestreamServer;

// Channel buffer sizes
const CHANNEL_SIZE: usize = 16;

/// Service managing live stream operations via gRPC.
#[derive(Debug)]
pub struct LiveStreamService {
    settings: Settings,

    stream_info_cache: MemoryCache<StreamInfo>,

    port_allocator: PortAllocator,

    control_tx: broadcast::Sender<StreamControlMessage>,
    stream_msg_tx: broadcast::Sender<StreamMessage>,
}

impl LiveStreamService {
    /// Creates a new LiveStreamService.
    ///
    /// # Arguments
    /// * `minio_client` - Client for uploading segments to MinIO
    /// * `settings` - Application settings
    pub fn new(minio_client: MinioClient, settings: Settings) -> Self {
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
        }
    }

    async fn make_stream_info(&self, live_id: &str, passphrase: &str) -> Result<StreamInfo> {
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

    async fn release_stream_resources(&self, info: StreamInfo) -> Result<()> {
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

        if !is_not_empty && let Err(e) = fs::remove_dir_all(info.cache_dir()).await {
            anyhow::bail!("Failed to remove cache directory: {e}");
        }

        Ok(())
    }

    pub async fn start_stream_impl(&self, stream_info: StreamInfo) -> Result<()> {
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
            stream_info.clone(),
        )
        .await;

        self.stream_info_cache.remove(&live_id).await;

        if let Err(e) = self.release_stream_resources(stream_info).await {
            warn!("Failed to release resources for stream {live_id}: {e}");
        }

        result
    }

    pub async fn stop_stream_impl(&self, live_id: &str) -> Result<()> {
        self.control_tx
            .send(StreamControlMessage::stop_stream(live_id))?;
        Ok(())
    }

    pub async fn list_active_streams_impl(&self) -> Result<Vec<String>> {
        Ok(self.stream_info_cache.keys().await)
    }

    pub async fn get_stream_info_impl(&self, live_id: String) -> Option<StreamInfo> {
        match self.stream_info_cache.get(&live_id).await {
            Some(info) => Some(info),
            None => {
                warn!("Failed to get stream info for live_id: {live_id}");
                None
            }
        }
    }
}

#[tonic::async_trait]
impl Livestream for Arc<LiveStreamService> {
    async fn start_pull_stream(
        &self,
        request: Request<StartPullStreamRequest>,
    ) -> Result<Response<StreamInfoResponse>, Status> {
        let request = request.into_inner();

        // Validate input
        if request.live_id.is_empty() {
            return Err(Status::invalid_argument("live_id cannot be empty"));
        }
        if request.passphrase.is_empty() {
            return Err(Status::invalid_argument("passphrase cannot be empty"));
        }
        regex::Regex::new(r"^[a-zA-Z0-9]{10,79}$")
            .unwrap()
            .is_match(&request.passphrase)
            .then_some(())
            .ok_or_else(|| {
                Status::invalid_argument("passphrase must be 10-79 alphanumeric characters")
            })?;

        let stream_info = match self
            .make_stream_info(&request.live_id, &request.passphrase)
            .await
        {
            Ok(info) => info,
            Err(e) => return Err(Status::resource_exhausted(e.to_string())),
        };

        let cloned_self = Arc::clone(self);
        let cloned_info = stream_info.clone();
        tokio::spawn(async move { cloned_self.start_stream_impl(cloned_info).await });

        let resp: StreamInfoResponse = stream_info.into();
        Ok(Response::new(resp))
    }

    async fn stop_pull_stream(
        &self,
        request: Request<StopPullStreamRequest>,
    ) -> Result<Response<StopPullStreamResponse>, Status> {
        let live_id = request.into_inner().live_id;

        // Validate input
        if live_id.is_empty() {
            return Err(Status::invalid_argument("live_id cannot be empty"));
        }

        let resp = StopPullStreamResponse {
            is_success: self.stop_stream_impl(&live_id).await.is_ok(),
        };
        Ok(Response::new(resp))
    }

    async fn list_active_streams(
        &self,
        _: Request<ListActiveStreamsRequest>,
    ) -> Result<Response<ListActiveStreamsResponse>, Status> {
        let resp = self
            .list_active_streams_impl()
            .await
            .map(|live_ids| ListActiveStreamsResponse { live_ids });

        match resp {
            Ok(response) => Ok(Response::new(response)),
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    async fn get_stream_info(
        &self,
        request: Request<GetStreamInfoRequest>,
    ) -> Result<Response<StreamInfoResponse>, Status> {
        let live_id = request.into_inner().live_id;

        // Validate input
        if live_id.is_empty() {
            return Err(Status::invalid_argument("live_id cannot be empty"));
        }

        if let Some(info) = self.get_stream_info_impl(live_id).await {
            let resp: StreamInfoResponse = info.into();
            Ok(Response::new(resp))
        } else {
            Err(Status::not_found("Stream not found"))
        }
    }
}

impl Into<StreamInfoResponse> for StreamInfo {
    fn into(self) -> StreamInfoResponse {
        StreamInfoResponse {
            live_id: self.live_id().to_string(),
            host: self.host().to_string(),
            srt_port: self.srt_port() as u32,
            rtmp_url: self.rtmp_pull_url().to_string(),
            passphrase: self.passphrase().to_string(),
        }
    }
}
