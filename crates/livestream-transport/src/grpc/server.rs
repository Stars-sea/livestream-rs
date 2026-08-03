use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio_util::sync::CancellationToken;
use tonic::codegen::tokio_stream;
use tonic::service::InterceptorLayer;
use tonic::transport::Server;
use tonic::{Request, Response, Status};
use tracing::{info, instrument, warn};

use super::api;
use super::api::livestream_server::LivestreamServer;
use crate::controller::TransportController;
use crate::dispatcher::{EventDispatcher, SessionEvent};
use crate::http_flv::playback_path;
use crate::registry::SessionRegistry;
use crate::registry::state::*;
use livestream_core::channel::BroadcastRx;
use livestream_core::types::Protocol;

const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Upper bound for waiting on the precreate registration ack. The ack is
/// resolved by the transport's control loop as soon as the message is
/// processed; this only guards against a wedged or shutting-down server.
const PRECREATE_ACK_TIMEOUT: Duration = Duration::from_secs(5);

const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("livestream_descriptor");

#[allow(dead_code)]
pub struct GrpcServerConfig {
    pub port: u16,
    pub rtmp_port: Option<u16>,
    pub rtmp_app_name: String,
    pub rtsp_port: Option<u16>,
    pub http_flv_enabled: bool,
    pub http_flv_port: u16,
    pub control: Arc<TransportController>,
    pub registry: Arc<SessionRegistry>,
    pub dispatcher: Arc<EventDispatcher>,
}

#[allow(dead_code)]
pub struct GrpcServer {
    port: u16,
    registry: Arc<SessionRegistry>,
    dispatcher: Arc<EventDispatcher>,
    service: IngestGrpcService,
    reflection_desc: Vec<u8>,
}

impl GrpcServer {
    pub fn new(cfg: GrpcServerConfig) -> Result<Self> {
        let port = cfg.port;
        let registry = cfg.registry.clone();
        let dispatcher = cfg.dispatcher.clone();
        Ok(Self {
            port,
            registry: registry.clone(),
            dispatcher: dispatcher.clone(),
            reflection_desc: FILE_DESCRIPTOR_SET.to_vec(),
            service: IngestGrpcService::new(cfg, registry, dispatcher),
        })
    }

    #[instrument(
        name = "server.grpc.serve",
        skip(self, shutdown),
        fields(server.port = self.port)
    )]
    pub async fn serve(self, shutdown: CancellationToken) -> Result<()> {
        let addr = format!("0.0.0.0:{}", self.port).parse()?;
        info!(address = %self.port, "gRPC Server will listen");

        let reflection = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(&self.reflection_desc)
            .build_v1()
            .map_err(|e| anyhow::anyhow!("Failed to build gRPC reflection descriptor: {e}"))?;

        let service = LivestreamServer::new(self.service);

        // Optional bearer-token auth for the control plane. When the
        // GRPC__AUTH_TOKEN env var is set, every request (including
        // reflection) must carry `authorization: Bearer <token>`; requests
        // without it are rejected with UNAUTHENTICATED. When unset the
        // server keeps its current open behavior.
        let auth_token: Option<String> = std::env::var("GRPC__AUTH_TOKEN")
            .ok()
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty());
        let auth_layer = InterceptorLayer::new(move |req: Request<()>| {
            let Some(token) = auth_token.as_deref() else {
                return Ok(req);
            };
            let expected = format!("Bearer {token}");
            let provided = req
                .metadata()
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .trim();
            if provided == expected {
                Ok(req)
            } else {
                Err(Status::unauthenticated("missing or invalid bearer token"))
            }
        });

        Server::builder()
            .layer(auth_layer)
            .add_service(reflection)
            .add_service(service)
            .serve_with_shutdown(addr, shutdown.cancelled())
            .await?;

        Ok(())
    }
}

#[derive(Clone)]
struct IngestGrpcService {
    control: Arc<TransportController>,
    registry: Arc<SessionRegistry>,
    dispatcher: Arc<EventDispatcher>,
    rtmp_port: Option<u16>,
    rtmp_app_name: String,
    rtsp_port: Option<u16>,
    http_flv_enabled: bool,
    http_flv_port: u16,
    grpc_port: u16,
}

impl IngestGrpcService {
    fn new(
        cfg: GrpcServerConfig,
        registry: Arc<SessionRegistry>,
        dispatcher: Arc<EventDispatcher>,
    ) -> Self {
        Self {
            control: cfg.control,
            registry,
            dispatcher,
            rtmp_port: cfg.rtmp_port,
            rtmp_app_name: cfg.rtmp_app_name,
            rtsp_port: cfg.rtsp_port,
            grpc_port: cfg.port,
            http_flv_enabled: cfg.http_flv_enabled,
            http_flv_port: cfg.http_flv_port,
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

    async fn await_precreate_ack(
        ack: crossfire::oneshot::RxOneshot<Result<SessionDescriptor>>,
    ) -> Result<SessionDescriptor, Status> {
        let res = tokio::time::timeout(PRECREATE_ACK_TIMEOUT, ack.recv_async())
            .await
            .map_err(|_| Status::internal("timed out waiting for precreate acknowledgment"))?
            .map_err(|_| {
                Status::internal("failed to receive acknowledgment from transport controller")
            })?;

        res.map_err(|e| {
            let message = e.to_string();
            if message.contains("already in use") {
                Status::already_exists("stream already exists")
            } else {
                Status::internal(format!(
                    "transport controller failed to precreate session: {message}"
                ))
            }
        })
    }

    async fn await_stop_ack(
        ack: crossfire::oneshot::RxOneshot<Result<()>>,
        live_id: &str,
    ) -> Result<bool, Status> {
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
        registry: &SessionRegistry,
    ) -> Option<SessionState> {
        loop {
            match registry.get_state(live_id).await {
                Some(state) if previous_state != Some(state) => return Some(state),
                Some(SessionState::Disconnected) => return None,
                Some(_) => {}
                None if previous_state == Some(SessionState::Disconnected) => return None,
                None => {
                    warn!(
                        live_id = %live_id,
                        "Stream disappeared without disconnect event, treating as disconnected"
                    );
                    return Some(SessionState::Disconnected);
                }
            }

            match tokio::time::timeout(WATCH_POLL_INTERVAL, subscription.recv()).await {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => {
                    // Channel closed or timeout; avoid busy-spin by sleeping.
                    tokio::time::sleep(WATCH_POLL_INTERVAL).await;
                }
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

        if self.registry.get_session(&live_id).is_some() {
            return Err(Status::already_exists("stream already exists"));
        }

        let protocol = Self::parse_input_protocol(request.input_protocol)?;

        let ack = match protocol {
            api::InputProtocol::Rtmp => self
                .control
                .precreate_rtmp_session(live_id.clone())
                .map_err(|e| Status::internal(e.to_string()))?,
            api::InputProtocol::Rtsp => self
                .control
                .precreate_rtsp_session(live_id.clone(), request.passphrase)
                .map_err(|e| Status::internal(e.to_string()))?,
            api::InputProtocol::Unspecified => {
                return Err(Status::invalid_argument(
                    "input_protocol must be specified (RTMP=1, RTSP=2)",
                ));
            }
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

        if self.registry.get_session(&live_id).is_none() {
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
        let streams = self
            .registry
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

        let descriptor = self
            .registry
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
        let dispatcher = self.dispatcher.clone();
        let registry = self.registry.clone();

        let stream = async_stream::try_stream! {
            let mut previous_state = None;
            let mut subscription = dispatcher.subscribe(&live_id);

            while let Some(state) = Self::wait_for_next_state(&live_id, previous_state, &mut subscription, &registry).await {
                previous_state = Some(state);
                yield Self::watch_response(state);

                if state == SessionState::Disconnected {
                    break;
                }
            }
        };

        Ok(Response::new(Box::pin(stream)))
    }

    #[instrument(name = "transport.grpc.get_service_info", err, skip(self, _request))]
    async fn get_service_info(
        &self,
        _request: Request<api::GetServiceInfoRequest>,
    ) -> Result<Response<api::GetServiceInfoResponse>, Status> {
        Ok(Response::new(api::GetServiceInfoResponse {
            grpc_port: self.grpc_port as u32,
            rtmp_port: self.rtmp_port.map_or(0, |p| p as u32),
            rtsp_port: self.rtsp_port.map_or(0, |p| p as u32),
            http_flv_port: self.http_flv_port().unwrap_or(0),
        }))
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
            Protocol::Rtmp => api::InputProtocol::Rtmp as i32,
            Protocol::Rtsp => api::InputProtocol::Rtsp as i32,
            other => {
                warn!(protocol = %other, live_id = %descriptor.id, "Descriptor with unknown protocol mapped to RTMP in gRPC response");
                api::InputProtocol::Rtmp as i32
            }
        };

        let status = Self::session_state_to_proto(descriptor.state);
        let protocol = descriptor.protocol;
        let live_id = descriptor.id;
        let rtmp_port = self.rtmp_port.unwrap_or(0) as u32;
        let ingest_port = descriptor.endpoint.port.map(u32::from).unwrap_or(rtmp_port);
        let http_flv_port = self.http_flv_port();
        let http_flv_path = self.http_flv_path(&live_id);

        api::StreamDescriptor {
            live_id: live_id.clone(),
            input_protocol,
            status,
            endpoints: Some(api::StreamEndpoints {
                ingest: Some(self.ingest_endpoints(&live_id, protocol, ingest_port)),
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
        _ingest_port: u32,
    ) -> api::IngestEndpoints {
        match protocol {
            Protocol::Rtmp => api::IngestEndpoints {
                rtmp: Some(api::RtmpEndpoint {
                    port: self.rtmp_port.unwrap_or(0) as u32,
                    app_name: self.rtmp_app_name.clone(),
                    stream_key: live_id.to_owned(),
                }),
                srt: None,
                rtsp: None,
            },
            Protocol::Rtsp => api::IngestEndpoints {
                rtmp: None,
                srt: None,
                rtsp: Some(api::RtspEndpoint {
                    port: self.rtsp_port.unwrap_or(0) as u32,
                    path: format!("/live/{}", live_id),
                }),
            },
            other => {
                warn!(protocol = %other, live_id = %live_id, "Ingest endpoints requested for unsupported protocol, falling back to RTMP");
                api::IngestEndpoints {
                    rtmp: Some(api::RtmpEndpoint {
                        port: self.rtmp_port.unwrap_or(0) as u32,
                        app_name: self.rtmp_app_name.clone(),
                        stream_key: live_id.to_owned(),
                    }),
                    srt: None,
                    rtsp: None,
                }
            }
        }
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
                app_name: self.rtmp_app_name.clone(),
                stream_key: live_id.to_owned(),
            }),
            http_flv: http_flv_path
                .zip(http_flv_port)
                .map(|(path, port)| api::HttpFlvPlaybackEndpoint { port, path }),
        }
    }

    fn http_flv_port(&self) -> Option<u32> {
        self.http_flv_enabled.then_some(self.http_flv_port as u32)
    }

    fn http_flv_path(&self, live_id: &str) -> Option<String> {
        self.http_flv_enabled.then(|| playback_path(live_id))
    }
}
