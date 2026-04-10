use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Result;
use regex::Regex;
use tokio::sync::Mutex;
use tokio::time::{Instant, sleep};
use tokio_util::sync::CancellationToken;
use tonic::codegen::tokio_stream;
use tonic::transport::Server;
use tonic::{Request, Response, Status};
use tracing::{Span, info, instrument, warn};

#[cfg(feature = "opentelemetry")]
use opentelemetry::propagation::Extractor;
#[cfg(feature = "opentelemetry")]
use tonic::metadata::{KeyRef, MetadataMap};
#[cfg(feature = "opentelemetry")]
use tracing_opentelemetry::OpenTelemetrySpanExt;

use super::api;
use super::api::livestream_server::LivestreamServer;
use crate::config::{GrpcConfig, RtmpConfig};
use crate::transport::TransportController;
use crate::transport::contract::state::*;
use crate::transport::registry::global;

static PASSPHRASE_REGEX: OnceLock<Regex> = OnceLock::new();
const DESCRIPTOR_READY_TIMEOUT: Duration = Duration::from_secs(2);
const SESSION_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

pub struct GrpcServer {
    grpc_config: GrpcConfig,
    service: IngestGrpcService,
}

#[cfg(feature = "opentelemetry")]
struct MetadataMapExtractor<'a>(&'a MetadataMap);

impl GrpcServer {
    pub fn new(
        grpc_config: GrpcConfig,
        rtmp_config: RtmpConfig,
        control: Arc<Mutex<TransportController>>,
    ) -> Self {
        Self {
            grpc_config,
            service: IngestGrpcService::new(control, rtmp_config),
        }
    }

    #[instrument(
        name = "server.grpc.serve",
        skip(self, shutdown),
        fields(server.port = self.grpc_config.port)
    )]
    pub async fn serve(self, shutdown: CancellationToken) -> Result<()> {
        let addr = format!("0.0.0.0:{}", self.grpc_config.port).parse()?;
        info!(address = %self.grpc_config.port, "gRPC Server will listen");

        let service = LivestreamServer::with_interceptor(self.service, server_trace_interceptor);

        Server::builder()
            .add_service(service)
            .serve_with_shutdown(addr, shutdown.cancelled())
            .await?;

        Ok(())
    }
}

#[cfg(feature = "opentelemetry")]
impl Extractor for MetadataMapExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0
            .keys()
            .map(|key| match key {
                KeyRef::Ascii(key) => key.as_str(),
                KeyRef::Binary(key) => key.as_str(),
            })
            .collect()
    }
}

fn server_trace_interceptor(request: Request<()>) -> Result<Request<()>, Status> {
    #[cfg(feature = "opentelemetry")]
    {
        let ctx = opentelemetry::global::get_text_map_propagator(|prop| {
            let extractor = MetadataMapExtractor(request.metadata());
            prop.extract(&extractor)
        });
        let _ = Span::current().set_parent(ctx);
    }

    Ok(request)
}

#[derive(Clone)]
struct IngestGrpcService {
    control: Arc<Mutex<TransportController>>,
    rtmp_config: RtmpConfig,
}

impl IngestGrpcService {
    fn new(control: Arc<Mutex<TransportController>>, rtmp_config: RtmpConfig) -> Self {
        Self {
            control,
            rtmp_config,
        }
    }

    async fn wait_until<T, F, Fut>(&self, timeout: Duration, mut check: F) -> Option<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Option<T>>,
    {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(value) = check().await {
                return Some(value);
            }

            if Instant::now() >= deadline {
                return None;
            }

            sleep(POLL_INTERVAL).await;
        }
    }

    async fn wait_for_descriptor(
        &self,
        live_id: &str,
        timeout: Duration,
    ) -> Option<SessionDescriptor> {
        self.wait_until(timeout, || async {
            global::get_session_descriptor(live_id).await
        })
        .await
    }

    async fn wait_for_session_removed(&self, live_id: &str, timeout: Duration) -> bool {
        self.wait_until(timeout, || async {
            if global::get_session(live_id).await.is_none() {
                Some(())
            } else {
                None
            }
        })
        .await
        .is_some()
    }
}

#[tonic::async_trait]
impl api::livestream_server::Livestream for IngestGrpcService {
    type WatchLivestreamStream = Pin<
        Box<
            dyn tokio_stream::Stream<Item = Result<api::WatchLivestreamResponse, Status>>
                + Send
                + 'static,
        >,
    >;

    #[instrument(
        name = "transport.grpc.start_livestream",
        err,
        skip(self, request),
        fields(live_id = %request.get_ref().live_id)
    )]
    async fn start_livestream(
        &self,
        request: Request<api::StartLivestreamRequest>,
    ) -> Result<Response<api::StartLivestreamResponse>, Status> {
        let request = request.into_inner();
        let live_id = request.live_id;

        if live_id.is_empty() {
            return Err(Status::invalid_argument("live_id cannot be empty"));
        }

        if global::get_session(&live_id).await.is_some() {
            return Err(Status::already_exists("stream already exists"));
        }

        let protocol = api::InputProtocol::try_from(request.input_protocol)
            .map_err(|_| Status::invalid_argument("input_protocol is invalid"))?;

        match protocol {
            api::InputProtocol::Srt => {
                let passphrase = request
                    .passphrase
                    .ok_or_else(|| Status::invalid_argument("passphrase cannot be empty"))?;
                let re = PASSPHRASE_REGEX.get_or_init(|| {
                    Regex::new(r"^[a-zA-Z0-9]{10,79}$").expect("invalid regex pattern")
                });
                if !re.is_match(&passphrase) {
                    return Err(Status::invalid_argument(
                        "passphrase must be 10-79 alphanumeric characters",
                    ));
                }

                self.control
                    .lock()
                    .await
                    .precreate_srt_session(live_id.clone(), passphrase)
                    .map_err(|e| Status::internal(e.to_string()))?;
            }
            api::InputProtocol::Rtmp => {
                self.control
                    .lock()
                    .await
                    .precreate_rtmp_session(live_id.clone())
                    .map_err(|e| Status::internal(e.to_string()))?;
            }
        }

        let descriptor = self
            .wait_for_descriptor(&live_id, DESCRIPTOR_READY_TIMEOUT)
            .await;

        let descriptor = match descriptor {
            Some(descriptor) => descriptor,
            None => {
                if let Err(e) = self.control.lock().await.close_session(live_id.clone()) {
                    warn!(live_id = %live_id, error = %e, "failed to cleanup timed-out stream session");
                }

                if !self
                    .wait_for_session_removed(&live_id, SESSION_CLEANUP_TIMEOUT)
                    .await
                {
                    warn!(live_id = %live_id, "timed out waiting for session cleanup after start timeout");
                }

                return Err(Status::deadline_exceeded(
                    "stream descriptor creation timed out",
                ));
            }
        };

        Ok(Response::new(api::StartLivestreamResponse {
            stream: Some(self.descriptor_to_proto(descriptor)),
        }))
    }

    #[instrument(
        name = "transport.grpc.stop_livestream",
        err,
        skip(self, request),
        fields(live_id = %request.get_ref().live_id)
    )]
    async fn stop_livestream(
        &self,
        request: Request<api::StopLivestreamRequest>,
    ) -> Result<Response<api::StopLivestreamResponse>, Status> {
        let live_id = request.into_inner().live_id;

        if live_id.is_empty() {
            return Err(Status::invalid_argument("live_id cannot be empty"));
        }

        if global::get_session(&live_id).await.is_none() {
            return Err(Status::not_found("stream not found"));
        }

        self.control
            .lock()
            .await
            .close_session(live_id.clone())
            .map_err(|e| Status::internal(e.to_string()))?;

        let is_success = self
            .wait_for_session_removed(&live_id, SESSION_CLEANUP_TIMEOUT)
            .await;

        if !is_success {
            warn!(live_id = %live_id, "timed out waiting for stream cleanup after stop request");
        }

        Ok(Response::new(api::StopLivestreamResponse { is_success }))
    }

    #[instrument(
        name = "transport.grpc.list_livestreams",
        err,
        skip(self, _request),
        fields(live_id = "")
    )]
    async fn list_livestreams(
        &self,
        _request: Request<api::ListLivestreamsRequest>,
    ) -> Result<Response<api::ListLivestreamsResponse>, Status> {
        let streams = global::list_session_descriptors()
            .await
            .into_iter()
            .map(|descriptor| self.descriptor_to_proto(descriptor))
            .collect();

        Ok(Response::new(api::ListLivestreamsResponse { streams }))
    }

    #[instrument(
        name = "transport.grpc.get_livestream_info",
        err,
        skip(self, request),
        fields(live_id = %request.get_ref().live_id)
    )]
    async fn get_livestream_info(
        &self,
        request: Request<api::GetLivestreamInfoRequest>,
    ) -> Result<Response<api::GetLivestreamInfoResponse>, Status> {
        let live_id = request.into_inner().live_id;

        if live_id.is_empty() {
            return Err(Status::invalid_argument("live_id cannot be empty"));
        }

        let descriptor = global::get_session_descriptor(&live_id)
            .await
            .ok_or_else(|| Status::not_found("stream not found"))?;

        Ok(Response::new(api::GetLivestreamInfoResponse {
            stream: Some(self.descriptor_to_proto(descriptor)),
        }))
    }

    #[instrument(
        name = "transport.grpc.watch_livestream",
        err,
        skip(self, request),
        fields(live_id = %request.get_ref().live_id)
    )]
    async fn watch_livestream(
        &self,
        request: Request<api::WatchLivestreamRequest>,
    ) -> Result<Response<Self::WatchLivestreamStream>, Status> {
        let live_id = request.into_inner().live_id;

        if live_id.is_empty() {
            return Err(Status::invalid_argument("live_id cannot be empty"));
        }

        let stream = async_stream::try_stream! {
            while let Some(state) = global::get_session_state(&live_id).await {
                yield api::WatchLivestreamResponse {
                    stream: Self::session_state_to_proto(state),
                };

                sleep(POLL_INTERVAL).await;
            }
        };

        Ok(Response::new(Box::pin(stream)))
    }
}

impl IngestGrpcService {
    fn session_state_to_proto(state: SessionState) -> i32 {
        match state {
            SessionState::Rtmp(RtmpState::Pending) => api::SessionStatus::Pending as i32,
            SessionState::Rtmp(RtmpState::Connecting) => api::SessionStatus::Connecting as i32,
            SessionState::Rtmp(RtmpState::Connected) => api::SessionStatus::Connected as i32,
            SessionState::Rtmp(RtmpState::Disconnected) => api::SessionStatus::Disconnected as i32,
            SessionState::Srt(SrtState::Pending) => api::SessionStatus::Pending as i32,
            SessionState::Srt(SrtState::Connected) => api::SessionStatus::Connected as i32,
            SessionState::Srt(SrtState::Disconnected) => api::SessionStatus::Disconnected as i32,
        }
    }

    fn descriptor_to_proto(&self, descriptor: SessionDescriptor) -> api::StreamDescriptor {
        let input_protocol = match descriptor.protocol {
            SessionProtocol::Srt => api::InputProtocol::Srt as i32,
            SessionProtocol::Rtmp => api::InputProtocol::Rtmp as i32,
        };

        let status = Self::session_state_to_proto(descriptor.state);

        let rtmp_port = self.rtmp_config.port as u32;
        let port = descriptor.endpoint.port.map(u32::from).unwrap_or(rtmp_port);

        api::StreamDescriptor {
            live_id: descriptor.id,
            input_protocol,
            status,
            endpoint: Some(api::StreamEndpoint {
                port,
                rtmp_port,
                rtmp_appname: self.rtmp_config.appname.clone(),
                passphrase: descriptor.endpoint.passphrase,
            }),
        }
    }
}
