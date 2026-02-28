use std::fmt::Display;
use std::sync::Arc;

use anyhow::Result;
use opentelemetry::global;
use opentelemetry::propagation::{Extractor, Injector};
use tokio::signal;
use tonic::metadata::{MetadataKey, MetadataMap, MetadataValue};
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Server};
use tonic::{Request, Status};
use tracing::{Span, info, instrument};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::settings::IngestConfig;

use super::grpc::livestream_callback_client::LivestreamCallbackClient;
use super::{LivestreamServer, LivestreamService, StreamManager};

struct MetadataInjector<'a>(&'a mut MetadataMap);

impl Injector for MetadataInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        let Ok(metadata_key) = MetadataKey::from_bytes(key.as_bytes()) else {
            return;
        };
        let Ok(metadata_value) = MetadataValue::try_from(value) else {
            return;
        };

        self.0.insert(metadata_key, metadata_value);
    }
}

struct MetadataExtractor<'a>(&'a MetadataMap);

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

#[derive(Clone, Default)]
pub(crate) struct TraceContextInterceptor;

impl Interceptor for TraceContextInterceptor {
    fn call(&mut self, mut request: Request<()>) -> std::result::Result<Request<()>, Status> {
        let context = Span::current().context();
        global::get_text_map_propagator(|propagator| {
            propagator.inject_context(&context, &mut MetadataInjector(request.metadata_mut()));
        });

        Ok(request)
    }
}

pub(crate) type CallbackClient =
    LivestreamCallbackClient<InterceptedService<Channel, TraceContextInterceptor>>;

pub struct GrpcClientFactory {
    url: String,
}

impl GrpcClientFactory {
    pub fn new(url: String) -> Self {
        Self { url }
    }

    pub async fn build(&self) -> Result<Option<CallbackClient>> {
        if self.url.is_empty() {
            return Ok(None);
        }

        let channel = Channel::from_shared(self.url.clone())?.connect().await?;
        let client =
            LivestreamCallbackClient::with_interceptor(channel, TraceContextInterceptor::default());

        Ok(Some(client))
    }
}

impl Default for GrpcClientFactory {
    fn default() -> Self {
        Self::new("".to_string())
    }
}

impl Display for GrpcClientFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("GrpcClientFactory {{ {} }}", &self.url))
    }
}

pub struct GrpcServerFactory {
    livestream_service: Option<LivestreamService>,
    manger: Option<Arc<StreamManager>>,
    config: Option<IngestConfig>,
}

impl GrpcServerFactory {
    pub fn new() -> Self {
        Self {
            livestream_service: None,
            manger: None,
            config: None,
        }
    }

    pub fn with_service(mut self, service: LivestreamService) -> Self {
        self.livestream_service = Some(service);
        self
    }

    pub fn with_manager(mut self, manager: Arc<StreamManager>) -> Self {
        self.manger = Some(manager);
        self
    }

    pub fn with_config(mut self, config: IngestConfig) -> Self {
        self.config = Some(config);
        self
    }

    #[instrument(name = "ingest.grpc.serve", skip(self), fields(server.port = %self.config.as_ref().map(|c| c.port).unwrap_or_default()))]
    pub async fn serve(self) -> Result<()> {
        let service = self
            .livestream_service
            .ok_or_else(|| anyhow::anyhow!("LivestreamService is required"))?;

        let manager = self
            .manger
            .ok_or_else(|| anyhow::anyhow!("StreamManager is required"))?;

        let config = self
            .config
            .ok_or_else(|| anyhow::anyhow!("IngestConfig is required"))?;

        let grpc_addr = format!("0.0.0.0:{}", config.port);
        info!(address = %grpc_addr, "gRPC Server will listen");

        Server::builder()
            .add_service(LivestreamServer::with_interceptor(
                service,
                Self::server_trace_interceptor,
            ))
            .serve_with_shutdown(grpc_addr.parse()?, shutdown_signal(manager))
            .await?;

        Ok(())
    }

    fn server_trace_interceptor(request: Request<()>) -> Result<Request<()>, Status> {
        let parent_cx = global::get_text_map_propagator(|prop| {
            prop.extract(&MetadataExtractor(request.metadata()))
        });

        // Set parent context for the trace, linking it with incoming grpc calls
        let _ = Span::current().set_parent(parent_cx);

        Ok(request)
    }
}

/// Handles graceful shutdown on SIGINT (Ctrl+C) or SIGTERM
async fn shutdown_signal(manager: Arc<StreamManager>) {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            info!("Received Ctrl+C signal, shutting down gracefully");
        },
        _ = terminate => {
            info!("Received SIGTERM signal, shutting down gracefully");
        },
    }

    manager.shutdown().await;

    while !manager.is_streams_empty().await {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}
