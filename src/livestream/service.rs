//! Live stream service managing SRT stream pulling and processing.

use super::events::*;
use super::grpc::livestream_server::Livestream;
use super::grpc::*;
use super::port_allocator::PortAllocator;
use super::pull_stream::pull_srt_loop;
use super::stream_info::StreamInfo;

use crate::livestream::handlers;
use crate::services::redis::RedisClient;
use crate::settings::Settings;

use std::sync::Arc;

use anyhow::Result;
use log::{info, warn};
use tokio::fs;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReadDirStream;
use tonic::{Request, Response, Status};

pub use super::grpc::livestream_server::LivestreamServer;

// Channel buffer sizes
const STOP_STREAM_CHANNEL_SIZE: usize = 16;
const STREAM_EVENT_CHANNEL_SIZE: usize = 16;

/// Service managing live stream operations via gRPC.
#[derive(Debug)]
pub struct LiveStreamService {
    settings: Settings,

    redis_client: RedisClient,

    port_allocator: PortAllocator,

    stop_stream_tx: StopStreamTx,

    segment_complete_tx: SegmentCompleteTx,

    stream_connected_tx: StreamConnectedTx,
    stream_terminate_tx: StreamTerminateTx,
}

impl LiveStreamService {
    /// Creates a new LiveStreamService.
    ///
    /// # Arguments
    /// * `redis_client` - Client for caching active stream info to Redis
    /// * `minio_client` - Client for uploading segments to MinIO
    /// * `settings` - Application settings
    pub fn new(
        redis_client: RedisClient,
        segment_complete_tx: SegmentCompleteTx,
        settings: Settings,
    ) -> Self {
        let (stop_stream_tx, _) = OnStopStream::channel(STOP_STREAM_CHANNEL_SIZE);

        let (stream_connected_tx, stream_connected_rx) =
            OnStreamConnected::channel(STREAM_EVENT_CHANNEL_SIZE);
        let (stream_terminate_tx, stream_terminate_rx) =
            OnStreamTerminate::channel(STREAM_EVENT_CHANNEL_SIZE);

        tokio::spawn(handlers::stream_connected_handler(
            stream_connected_rx,
            redis_client.clone(),
        ));
        tokio::spawn(handlers::stream_terminate_handler(
            stream_terminate_rx,
            redis_client.clone(),
        ));

        let port_allocator = {
            let (start_port, end_port) = settings
                .srt_port_range()
                .expect("Invalid SRT port range in settings");
            PortAllocator::new(start_port, end_port)
        };

        Self {
            settings,
            redis_client,
            port_allocator,
            stop_stream_tx,
            segment_complete_tx,
            stream_connected_tx,
            stream_terminate_tx,
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
        self.port_allocator.release_port(info.port()).await;

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

    async fn start_stream_impl(&self, stream_info: StreamInfo) -> Result<()> {
        let live_id = stream_info.live_id().to_string();
        info!(
            "Ready to pull stream at port {} (LiveId: {live_id})",
            stream_info.port()
        );

        self.redis_client
            .cache_stream_info(stream_info.clone())
            .await?;

        let result = pull_srt_loop(
            self.stream_connected_tx.clone(),
            self.stream_terminate_tx.clone(),
            self.segment_complete_tx.clone(),
            self.stop_stream_tx.subscribe(),
            &stream_info,
        );

        self.redis_client.remove_stream_info(&live_id).await?;

        if let Err(e) = self.release_stream_resources(stream_info).await {
            warn!("Failed to release resources for stream {live_id}: {e}");
        }

        result
    }

    async fn stop_stream_impl(&self, live_id: &str) -> Result<()> {
        self.stop_stream_tx.send(OnStopStream::new(live_id))?;
        Ok(())
    }

    async fn list_active_streams_impl(&self) -> Result<Vec<String>> {
        self.redis_client.get_live_ids().await
    }

    async fn get_stream_info_impl(&self, live_id: String) -> Option<StreamInfo> {
        match self.redis_client.find_stream_info(&live_id).await {
            Ok(info) => info,
            Err(e) => {
                warn!("Failed to get stream info: {e}");
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
    ) -> Result<Response<StartPullStreamResponse>, Status> {
        let request = request.into_inner();

        // Validate input
        if request.live_id.is_empty() {
            return Err(Status::invalid_argument("live_id cannot be empty"));
        }
        if request.passphrase.is_empty() {
            return Err(Status::invalid_argument("passphrase cannot be empty"));
        }

        let stream_info = match self
            .make_stream_info(&request.live_id, &request.passphrase)
            .await
        {
            Ok(info) => info,
            Err(e) => return Err(Status::resource_exhausted(e.to_string())),
        };

        let cloned_self = Arc::clone(self);
        tokio::spawn(async move { cloned_self.start_stream_impl(stream_info).await });

        if let Some(info) = self.get_stream_info_impl(request.live_id.clone()).await {
            let resp: StartPullStreamResponse = info.into();
            Ok(Response::new(resp))
        } else {
            Err(Status::internal("Failed to find pulling stream info"))
        }
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
    ) -> Result<Response<GetStreamInfoResponse>, Status> {
        let live_id = request.into_inner().live_id;

        // Validate input
        if live_id.is_empty() {
            return Err(Status::invalid_argument("live_id cannot be empty"));
        }

        if let Some(info) = self.get_stream_info_impl(live_id).await {
            let resp: GetStreamInfoResponse = info.into();
            Ok(Response::new(resp))
        } else {
            Err(Status::not_found("Stream not found"))
        }
    }
}

impl Into<StartPullStreamResponse> for StreamInfo {
    fn into(self) -> StartPullStreamResponse {
        StartPullStreamResponse {
            live_id: self.live_id().to_string(),
            port: self.port() as u32,
            passphrase: self.passphrase().to_string(),
        }
    }
}

impl Into<GetStreamInfoResponse> for StreamInfo {
    fn into(self) -> GetStreamInfoResponse {
        GetStreamInfoResponse {
            port: self.port() as u32,
            passphrase: self.passphrase().to_string(),
        }
    }
}
