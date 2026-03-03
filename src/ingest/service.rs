//! Live stream service managing SRT stream pulling and processing.

use crate::{ingest::stream_info::StreamInputOptions, otlp::metrics, settings::load_settings};

use super::grpc::livestream_server::Livestream;
use super::grpc::*;
use super::manager::StreamManager;
use super::stream_info::StreamInfo;

use std::sync::Arc;

use anyhow::Result;
use regex::Regex;
use std::sync::OnceLock;
use tonic::{Request, Response, Status};
use tracing::instrument;

pub use super::grpc::livestream_server::LivestreamServer;

static PASSPHRASE_REGEX: OnceLock<Regex> = OnceLock::new();

/// Service managing live stream operations via gRPC.
#[derive(Debug)]
pub struct LivestreamService {
    manager: Arc<StreamManager>,
}

impl LivestreamService {
    pub fn new(manager: Arc<StreamManager>) -> Self {
        Self { manager }
    }
}

#[tonic::async_trait]
impl Livestream for LivestreamService {
    #[instrument(name = "ingest.grpc.start_pull_stream", skip(self, request), fields(stream.live_id = %request.get_ref().live_id))]
    async fn start_pull_stream(
        &self,
        request: Request<StartPullStreamRequest>,
    ) -> Result<Response<StreamInfoResponse>, Status> {
        let mut grpc_guard = metrics::get_metrics().grpc_call("start_pull_stream");
        let request = request.into_inner();
        let protocol =
            InputProtocol::try_from(request.input_protocol).unwrap_or(InputProtocol::Srt);

        // Validate input
        if request.live_id.is_empty() {
            return Err(Status::invalid_argument("live_id cannot be empty"));
        }
        let stream_info = match protocol {
            InputProtocol::Srt => {
                if request.passphrase.as_ref().is_none_or(|p| p.is_empty()) {
                    return Err(Status::invalid_argument("passphrase cannot be empty"));
                }

                let passphrase = request.passphrase.as_ref().unwrap();
                let re = PASSPHRASE_REGEX.get_or_init(|| {
                    Regex::new(r"^[a-zA-Z0-9]{10,79}$").expect("invalid regex pattern")
                });

                re.is_match(passphrase).then_some(()).ok_or_else(|| {
                    Status::invalid_argument("passphrase must be 10-79 alphanumeric characters")
                })?;

                match self
                    .manager
                    .make_srt_stream_info(&request.live_id, passphrase)
                    .await
                {
                    Ok(info) => info,
                    Err(e) => return Err(Status::resource_exhausted(e.to_string())),
                }
            }
            InputProtocol::Rtmp => {
                match self.manager.make_rtmp_stream_info(&request.live_id).await {
                    Ok(info) => info,
                    Err(e) => return Err(Status::internal(e.to_string())),
                }
            }
        };

        if let Err(e) = self.manager.start_stream(stream_info.clone()).await {
            return Err(Status::internal(e.to_string()));
        }

        let resp: StreamInfoResponse = stream_info.into();
        grpc_guard.success();
        Ok(Response::new(resp))
    }

    #[instrument(name = "ingest.grpc.stop_pull_stream", skip(self, request), fields(stream.live_id = %request.get_ref().live_id))]
    async fn stop_pull_stream(
        &self,
        request: Request<StopPullStreamRequest>,
    ) -> Result<Response<StopPullStreamResponse>, Status> {
        let mut grpc_guard = metrics::get_metrics().grpc_call("stop_pull_stream");
        let live_id = request.into_inner().live_id;

        // Validate input
        if live_id.is_empty() {
            return Err(Status::invalid_argument("live_id cannot be empty"));
        }

        let resp = StopPullStreamResponse {
            is_success: self.manager.stop_stream(&live_id).await.is_ok(),
        };
        grpc_guard.success();
        Ok(Response::new(resp))
    }

    #[instrument(name = "ingest.grpc.list_active_streams", skip(self, request))]
    async fn list_active_streams(
        &self,
        request: Request<ListActiveStreamsRequest>,
    ) -> Result<Response<ListActiveStreamsResponse>, Status> {
        let mut grpc_guard = metrics::get_metrics().grpc_call("list_active_streams");
        let _ = request;
        let resp = self
            .manager
            .list_active_streams()
            .await
            .map(|live_ids| ListActiveStreamsResponse { live_ids });

        match resp {
            Ok(response) => {
                grpc_guard.success();
                Ok(Response::new(response))
            }
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    #[instrument(name = "ingest.grpc.get_stream_info", skip(self, request), fields(stream.live_id = %request.get_ref().live_id))]
    async fn get_stream_info(
        &self,
        request: Request<GetStreamInfoRequest>,
    ) -> Result<Response<StreamInfoResponse>, Status> {
        let mut grpc_guard = metrics::get_metrics().grpc_call("get_stream_info");
        let live_id = request.into_inner().live_id;

        // Validate input
        if live_id.is_empty() {
            return Err(Status::invalid_argument("live_id cannot be empty"));
        }

        if let Some(info) = self.manager.get_stream_info(&live_id).await {
            let resp: StreamInfoResponse = info.into();
            grpc_guard.success();
            Ok(Response::new(resp))
        } else {
            Err(Status::not_found("Stream not found"))
        }
    }
}

impl Into<StreamInfoResponse> for Arc<StreamInfo> {
    fn into(self) -> StreamInfoResponse {
        let settings = load_settings();

        match self.input_options() {
            StreamInputOptions::Srt(options) => StreamInfoResponse {
                live_id: self.live_id().to_string(),
                host: options.host().to_string(),
                port: options.port() as u32,
                pull_port: settings.publish.port as u32,
                rtmp_appname: settings.publish.appname.clone(),
                passphrase: Some(options.passphrase().to_string()),
                input_protocol: InputProtocol::Srt as i32,
            },
            StreamInputOptions::Rtmp(options) => StreamInfoResponse {
                live_id: self.live_id().to_string(),
                host: options.host().to_string(),
                port: options.port() as u32,
                pull_port: settings.publish.port as u32,
                rtmp_appname: settings.publish.appname.clone(),
                passphrase: None,
                input_protocol: InputProtocol::Rtmp as i32,
            },
        }
    }
}
