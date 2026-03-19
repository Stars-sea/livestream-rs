use std::sync::Arc;

use anyhow::Result;
use regex::Regex;
use std::sync::OnceLock;
use tokio_util::sync::CancellationToken;
use tonic::transport::Server;
use tonic::{Request, Response, Status};
use tracing::{info, instrument};

#[cfg(feature = "opentelemetry")]
use opentelemetry::global;
#[cfg(feature = "opentelemetry")]
use opentelemetry::propagation::Extractor;
#[cfg(feature = "opentelemetry")]
use tracing::Span;
#[cfg(feature = "opentelemetry")]
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::config::{EgressConfig, GrpcConfig};
use crate::infra::api;
use crate::ingest::StreamManager;
use crate::ingest::stream_info::{StreamInfo, StreamInputOptions};
use crate::telemetry::metrics;

static PASSPHRASE_REGEX: OnceLock<Regex> = OnceLock::new();

#[derive(Debug)]
pub struct IngestGrpcService {
    manager: Arc<StreamManager>,
    egress_config: EgressConfig,
}

impl IngestGrpcService {
    pub fn new(manager: Arc<StreamManager>, egress_config: EgressConfig) -> Self {
        Self { manager, egress_config }
    }

    fn to_response(&self, info: Arc<StreamInfo>) -> api::StreamInfoResponse {
        match info.input_options() {
            StreamInputOptions::Srt(options) => api::StreamInfoResponse {
                live_id: info.live_id().to_string(),
                host: options.host().to_string(),
                port: options.port() as u32,
                pull_port: self.egress_config.port as u32,
                rtmp_appname: self.egress_config.appname.clone(),
                passphrase: Some(options.passphrase().to_string()),
                input_protocol: api::InputProtocol::Srt as i32,
            },
            StreamInputOptions::Rtmp {
                host,
                port,
                appname,
            } => api::StreamInfoResponse {
                live_id: info.live_id().to_string(),
                host: host.clone(),
                port: *port as u32,
                pull_port: self.egress_config.port as u32,
                rtmp_appname: appname.clone(),
                passphrase: None,
                input_protocol: api::InputProtocol::Rtmp as i32,
            },
        }
    }
}

#[tonic::async_trait]
impl api::livestream_server::Livestream for IngestGrpcService {
    #[instrument(name = "ingest.grpc.start_stream", skip(self, request), fields(stream.live_id = %request.get_ref().live_id))]
    async fn start_ingest_stream(
        &self,
        request: Request<api::StartIngestStreamRequest>,
    ) -> Result<Response<api::StreamInfoResponse>, Status> {
        let mut grpc_guard = metrics::get_metrics().grpc_call("start_ingest_stream");
        let request = request.into_inner();
        let protocol =
            api::InputProtocol::try_from(request.input_protocol).unwrap_or(api::InputProtocol::Srt);

        if request.live_id.is_empty() {
            return Err(Status::invalid_argument("live_id cannot be empty"));
        }

        let stream_info = match protocol {
            api::InputProtocol::Srt => {
                if request.passphrase.as_ref().is_none_or(|p| p.is_empty()) {
                    return Err(Status::invalid_argument("passphrase cannot be empty"));
                }

                let passphrase = request.passphrase.as_ref().expect("passphrase checked");
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
            api::InputProtocol::Rtmp => {
                match self.manager.make_rtmp_stream_info(&request.live_id).await {
                    Ok(info) => info,
                    Err(e) => return Err(Status::internal(e.to_string())),
                }
            }
        };

        match protocol {
            api::InputProtocol::Srt => {
                if let Err(e) = self.manager.start_srt_stream(stream_info.clone()).await {
                    return Err(Status::internal(e.to_string()));
                }
            }
            api::InputProtocol::Rtmp => {
                if let Err(e) = self.manager.start_rtmp_stream(stream_info.clone()).await {
                    return Err(Status::internal(e.to_string()));
                }
            }
        }

        let resp = self.to_response(stream_info);
        grpc_guard.success();
        Ok(Response::new(resp))
    }

    #[instrument(name = "ingest.grpc.stop_stream", skip(self, request), fields(stream.live_id = %request.get_ref().live_id))]
    async fn stop_ingest_stream(
        &self,
        request: Request<api::StopIngestStreamRequest>,
    ) -> Result<Response<api::StopIngestStreamResponse>, Status> {
        let mut grpc_guard = metrics::get_metrics().grpc_call("stop_ingest_stream");
        let live_id = request.into_inner().live_id;

        if live_id.is_empty() {
            return Err(Status::invalid_argument("live_id cannot be empty"));
        }

        let resp = api::StopIngestStreamResponse {
            is_success: self.manager.stop_stream(&live_id).await.is_ok(),
        };
        grpc_guard.success();
        Ok(Response::new(resp))
    }

    #[instrument(name = "ingest.grpc.list_streams", skip(self, request))]
    async fn list_ingest_streams(
        &self,
        request: Request<api::ListIngestStreamsRequest>,
    ) -> Result<Response<api::ListIngestStreamsResponse>, Status> {
        let mut grpc_guard = metrics::get_metrics().grpc_call("list_ingest_streams");
        let _ = request;
        let resp = self
            .manager
            .list_active_streams()
            .await
            .map(|live_ids| api::ListIngestStreamsResponse { live_ids });

        match resp {
            Ok(response) => {
                grpc_guard.success();
                Ok(Response::new(response))
            }
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    #[instrument(name = "ingest.grpc.get_stream_info", skip(self, request), fields(stream.live_id = %request.get_ref().live_id))]
    async fn get_ingest_stream_info(
        &self,
        request: Request<api::GetIngestStreamInfoRequest>,
    ) -> Result<Response<api::StreamInfoResponse>, Status> {
        let mut grpc_guard = metrics::get_metrics().grpc_call("get_ingest_stream_info");
        let live_id = request.into_inner().live_id;

        if live_id.is_empty() {
            return Err(Status::invalid_argument("live_id cannot be empty"));
        }

        if let Some(info) = self.manager.get_stream_info(&live_id).await {
            let resp = self.to_response(info);
            grpc_guard.success();
            Ok(Response::new(resp))
        } else {
            Err(Status::not_found("Stream not found"))
        }
    }
}

#[cfg(feature = "opentelemetry")]
struct MetadataExtractor<'a>(&'a tonic::metadata::MetadataMap);

#[cfg(feature = "opentelemetry")]
impl Extractor for MetadataExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0
            .keys()
            .map(|key| match key {
                tonic::metadata::KeyRef::Ascii(k) => k.as_str(),
                tonic::metadata::KeyRef::Binary(k) => k.as_str(),
            })
            .collect()
    }
}

pub struct GrpcServer {
    manager: Arc<StreamManager>,
    grpc_config: GrpcConfig,
    egress_config: EgressConfig,
}

impl GrpcServer {
    pub fn new(manager: Arc<StreamManager>, grpc_config: GrpcConfig, egress_config: EgressConfig) -> Self {
        Self { manager, grpc_config, egress_config }
    }

    #[instrument(name = "server.grpc.serve", skip(self, shutdown), fields(server.port = self.grpc_config.port))]
    pub async fn serve(self, shutdown: CancellationToken) -> Result<()> {
        let grpc_addr = format!("0.0.0.0:{}", self.grpc_config.port);
        info!(address = %grpc_addr, "gRPC Server will listen");

        let service = IngestGrpcService::new(self.manager.clone(), self.egress_config);
        Server::builder()
            .add_service(api::livestream_server::LivestreamServer::with_interceptor(
                service,
                Self::server_trace_interceptor,
            ))
            .serve_with_shutdown(grpc_addr.parse()?, shutdown_signal(self.manager, shutdown))
            .await?;

        Ok(())
    }

    fn server_trace_interceptor(request: Request<()>) -> Result<Request<()>, Status> {
        #[cfg(feature = "opentelemetry")]
        {
            let parent_cx = global::get_text_map_propagator(|prop| {
                prop.extract(&MetadataExtractor(request.metadata()))
            });
            let _ = Span::current().set_parent(parent_cx);
        }

        Ok(request)
    }
}

async fn shutdown_signal(manager: Arc<StreamManager>, token: CancellationToken) {
    token.cancelled().await;
    info!("Shutdown signal received, draining streams...");

    manager.shutdown().await;

    while !manager.is_streams_empty().await {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    info!("All active streams stopped.");
}
