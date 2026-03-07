use std::fmt::Display;

use anyhow::Result;
use tonic::transport::Channel;

#[cfg(feature = "opentelemetry")]
use opentelemetry::global;
#[cfg(feature = "opentelemetry")]
use opentelemetry::propagation::Injector;
#[cfg(feature = "opentelemetry")]
use tonic::metadata::{MetadataKey, MetadataMap, MetadataValue};
#[cfg(feature = "opentelemetry")]
use tonic::service::Interceptor;
#[cfg(feature = "opentelemetry")]
use tonic::service::interceptor::InterceptedService;
#[cfg(feature = "opentelemetry")]
use tonic::{Request, Status};
#[cfg(feature = "opentelemetry")]
use tracing::Span;
#[cfg(feature = "opentelemetry")]
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::settings::load_settings;

use super::api::livestream_callback_client::LivestreamCallbackClient;

#[cfg(feature = "opentelemetry")]
struct MetadataInjector<'a>(&'a mut MetadataMap);

#[cfg(feature = "opentelemetry")]
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

#[cfg(feature = "opentelemetry")]
#[derive(Clone, Default)]
pub(crate) struct TraceContextInterceptor;

#[cfg(feature = "opentelemetry")]
impl Interceptor for TraceContextInterceptor {
    fn call(&mut self, request: Request<()>) -> std::result::Result<Request<()>, Status> {
        let mut request = request;
        let context = Span::current().context();
        global::get_text_map_propagator(|propagator| {
            propagator.inject_context(&context, &mut MetadataInjector(request.metadata_mut()));
        });

        Ok(request)
    }
}

#[cfg(feature = "opentelemetry")]
pub type CallbackClient =
    LivestreamCallbackClient<InterceptedService<Channel, TraceContextInterceptor>>;
#[cfg(not(feature = "opentelemetry"))]
pub type CallbackClient = LivestreamCallbackClient<Channel>;

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
        #[cfg(feature = "opentelemetry")]
        let client =
            LivestreamCallbackClient::with_interceptor(channel, TraceContextInterceptor::default());
        #[cfg(not(feature = "opentelemetry"))]
        let client = LivestreamCallbackClient::new(channel);

        Ok(Some(client))
    }
}

impl Default for GrpcClientFactory {
    fn default() -> Self {
        Self::new(load_settings().grpc.callback.clone())
    }
}

impl Display for GrpcClientFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("GrpcClientFactory {{ {} }}", &self.url))
    }
}
