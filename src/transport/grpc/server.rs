use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Result;
use crossfire::oneshot::RxOneshot;
use regex::Regex;
use tokio_util::sync::CancellationToken;
use tonic::codegen::tokio_stream;
use tonic::transport::Server;
use tonic::{Request, Response, Status};
use tracing::{info, instrument, warn};

use super::api;
use super::api::livestream_server::LivestreamServer;
#[cfg(feature = "opentelemetry")]
use super::context_propagation::OtelContextPropagationService;
use crate::channel::BroadcastRx;
use crate::config::{GrpcConfig, HttpFlvConfig, RtmpConfig};
use crate::dispatcher::{self, Protocol, SessionEvent};
use crate::transport::http_flv::playback_path;
use crate::transport::registry::state::*;
use crate::transport::{TransportController, registry};

static PASSPHRASE_REGEX: OnceLock<Regex> = OnceLock::new();
const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub struct GrpcServer {
    grpc_config: GrpcConfig,
    service: IngestGrpcService,
}

impl GrpcServer {
    pub fn new(
        grpc_config: GrpcConfig,
        rtmp_config: RtmpConfig,
        http_flv_config: HttpFlvConfig,
        control: Arc<TransportController>,
    ) -> Self {
        Self {
            grpc_config,
            service: IngestGrpcService::new(control, rtmp_config, http_flv_config),
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

        let service = LivestreamServer::new(self.service);

        #[cfg(feature = "opentelemetry")]
        let service = OtelContextPropagationService::new(service);

        Server::builder()
            .add_service(service)
            .serve_with_shutdown(addr, shutdown.cancelled())
            .await?;

        Ok(())
    }
}

#[derive(Clone)]
struct IngestGrpcService {
    control: Arc<TransportController>,
    rtmp_config: RtmpConfig,
    http_flv_config: HttpFlvConfig,
}

impl IngestGrpcService {
    fn new(
        control: Arc<TransportController>,
        rtmp_config: RtmpConfig,
        http_flv_config: HttpFlvConfig,
    ) -> Self {
        Self {
            control,
            rtmp_config,
            http_flv_config,
        }
    }

    fn validate_live_id(live_id: String) -> Result<String, Status> {
        if live_id.is_empty() {
            return Err(Status::invalid_argument("live_id cannot be empty"));
        }

        Ok(live_id)
    }

    fn parse_input_protocol(value: i32) -> Result<api::InputProtocol, Status> {
        api::InputProtocol::try_from(value)
            .map_err(|_| Status::invalid_argument("input_protocol is invalid"))
    }

    fn validate_srt_passphrase(passphrase: Option<String>) -> Result<String, Status> {
        let passphrase =
            passphrase.ok_or_else(|| Status::invalid_argument("passphrase cannot be empty"))?;
        let re = PASSPHRASE_REGEX
            .get_or_init(|| Regex::new(r"^[a-zA-Z0-9]{10,79}$").expect("invalid regex pattern"));

        if !re.is_match(&passphrase) {
            return Err(Status::invalid_argument(
                "passphrase must be 10-79 alphanumeric characters",
            ));
        }

        Ok(passphrase)
    }

    async fn await_precreate_ack(
        ack: RxOneshot<Result<SessionDescriptor>>,
    ) -> Result<SessionDescriptor, Status> {
        let res = ack.recv_async().await.map_err(|_| {
            Status::internal("failed to receive acknowledgment from transport controller")
        })?;

        res.map_err(|e| {
            Status::internal(format!(
                "transport controller failed to precreate session: {e}"
            ))
        })
    }

    async fn await_stop_ack(ack: RxOneshot<Result<()>>, live_id: &str) -> Result<bool, Status> {
        let res = ack.recv_async().await.map_err(|_| {
            Status::internal("failed to receive acknowledgment from transport controller")
        })?;

        let is_success = res.is_ok();
        if let Err(error) = res {
            warn!(live_id = %live_id, error = %error, "timed out waiting for stream cleanup after stop request");
        }

        Ok(is_success)
    }

    async fn wait_for_next_state(
        live_id: &str,
        previous_state: Option<SessionState>,
        subscription: &mut BroadcastRx<SessionEvent>,
    ) -> Option<SessionState> {
        loop {
            match registry::INSTANCE.get_state(live_id).await {
                Some(state) if previous_state != Some(state) => return Some(state),
                Some(SessionState::Disconnected) => return None,
                Some(_) => {}
                None => {
                    if previous_state == Some(SessionState::Disconnected) {
                        return None;
                    }

                    warn!(
                        live_id = %live_id,
                        "Stream disappeared without disconnect event, treating as disconnected"
                    );
                    return Some(SessionState::Disconnected);
                }
            }

            match tokio::time::timeout(WATCH_POLL_INTERVAL, subscription.next()).await {
                Ok(Some(_)) | Ok(None) | Err(_) => {}
            }
        }
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
        let live_id = Self::validate_live_id(request.live_id)?;

        if registry::INSTANCE.get_session(&live_id).is_some() {
            return Err(Status::already_exists("stream already exists"));
        }

        let protocol = Self::parse_input_protocol(request.input_protocol)?;

        let ack = match protocol {
            api::InputProtocol::Srt => {
                let passphrase = Self::validate_srt_passphrase(request.passphrase)?;

                self.control
                    .precreate_srt_session(live_id.clone(), passphrase)
                    .map_err(|e| Status::internal(e.to_string()))?
            }
            api::InputProtocol::Rtmp => self
                .control
                .precreate_rtmp_session(live_id.clone())
                .map_err(|e| Status::internal(e.to_string()))?,
        };

        let descriptor = Self::await_precreate_ack(ack).await?;

        Ok(Response::new(api::StartLivestreamResponse {
            descriptor: Some(self.descriptor_to_proto(descriptor)),
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
        let live_id = Self::validate_live_id(request.into_inner().live_id)?;

        if registry::INSTANCE.get_session(&live_id).is_none() {
            return Err(Status::not_found("stream not found"));
        }

        let ack = self
            .control
            .close_session(live_id.clone())
            .map_err(|e| Status::internal(e.to_string()))?;

        let is_success = Self::await_stop_ack(ack, &live_id).await?;

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
        let streams = registry::INSTANCE
            .list_descriptors()
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
        let live_id = Self::validate_live_id(request.into_inner().live_id)?;

        let descriptor = registry::INSTANCE
            .get_descriptor(&live_id)
            .await
            .ok_or_else(|| Status::not_found("stream not found"))?;

        Ok(Response::new(api::GetLivestreamInfoResponse {
            descriptor: Some(self.descriptor_to_proto(descriptor)),
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
        let live_id = Self::validate_live_id(request.into_inner().live_id)?;

        let stream = async_stream::try_stream! {
            let mut previous_state = None;
            let mut subscription = dispatcher::INSTANCE.subscribe(&live_id);

            while let Some(state) = Self::wait_for_next_state(&live_id, previous_state, &mut subscription).await {
                previous_state = Some(state);
                yield Self::watch_response(state);

                if state == SessionState::Disconnected {
                    break;
                }
            }
        };

        Ok(Response::new(Box::pin(stream)))
    }
}

impl IngestGrpcService {
    fn session_state_to_proto(state: SessionState) -> i32 {
        match state {
            SessionState::Pending => api::SessionStatus::Pending as i32,
            SessionState::Connecting => api::SessionStatus::Connecting as i32,
            SessionState::Connected => api::SessionStatus::Connected as i32,
            SessionState::Disconnected => api::SessionStatus::Disconnected as i32,
        }
    }

    fn watch_response(state: SessionState) -> api::WatchLivestreamResponse {
        api::WatchLivestreamResponse {
            status: Self::session_state_to_proto(state),
        }
    }

    fn descriptor_to_proto(&self, descriptor: SessionDescriptor) -> api::StreamDescriptor {
        let input_protocol = match descriptor.protocol {
            Protocol::Srt => api::InputProtocol::Srt as i32,
            Protocol::Rtmp => api::InputProtocol::Rtmp as i32,
        };

        let status = Self::session_state_to_proto(descriptor.state);
        let protocol = descriptor.protocol;
        let live_id = descriptor.id;
        let rtmp_port = self.rtmp_config.port as u32;
        let ingest_port = descriptor.endpoint.port.map(u32::from).unwrap_or(rtmp_port);
        let passphrase = descriptor.endpoint.passphrase;
        let http_flv_port = self.http_flv_port();
        let http_flv_path = self.http_flv_path(&live_id);

        api::StreamDescriptor {
            live_id: live_id.clone(),
            input_protocol,
            status,
            endpoints: Some(api::StreamEndpoints {
                ingest: Some(self.ingest_endpoints(&live_id, protocol, ingest_port, passphrase)),
                playback: Some(self.playback_endpoints(
                    &live_id,
                    rtmp_port,
                    http_flv_port,
                    http_flv_path,
                )),
            }),
        }
    }

    fn ingest_endpoints(
        &self,
        live_id: &str,
        protocol: Protocol,
        ingest_port: u32,
        passphrase: Option<String>,
    ) -> api::IngestEndpoints {
        let (rtmp, srt) = match protocol {
            Protocol::Rtmp => (
                Some(api::RtmpEndpoint {
                    port: self.rtmp_config.port as u32,
                    app_name: self.rtmp_config.app_name.clone(),
                    stream_key: live_id.to_owned(),
                }),
                None,
            ),
            Protocol::Srt => (
                None,
                Some(api::SrtIngestEndpoint {
                    port: ingest_port,
                    live_id: live_id.to_owned(),
                    passphrase,
                }),
            ),
        };

        api::IngestEndpoints { rtmp, srt }
    }

    fn playback_endpoints(
        &self,
        live_id: &str,
        rtmp_port: u32,
        http_flv_port: Option<u32>,
        http_flv_path: Option<String>,
    ) -> api::PlaybackEndpoints {
        api::PlaybackEndpoints {
            rtmp: Some(api::RtmpEndpoint {
                port: rtmp_port,
                app_name: self.rtmp_config.app_name.clone(),
                stream_key: live_id.to_owned(),
            }),
            http_flv: http_flv_path
                .zip(http_flv_port)
                .map(|(path, port)| api::HttpFlvPlaybackEndpoint { port, path }),
        }
    }

    fn http_flv_port(&self) -> Option<u32> {
        self.http_flv_config
            .enabled
            .then_some(self.http_flv_config.port as u32)
    }

    fn http_flv_path(&self, live_id: &str) -> Option<String> {
        self.http_flv_config.enabled.then(|| playback_path(live_id))
    }
}
