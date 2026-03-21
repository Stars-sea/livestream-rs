use std::fmt::Display;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;
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

use crate::infra::api::livestream_callback_client::LivestreamCallbackClient;

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
    channel: Arc<RwLock<Option<Channel>>>,
}

impl GrpcClientFactory {
    pub fn new(url: String) -> Self {
        Self {
            url,
            channel: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn build(&self) -> Result<Option<CallbackClient>> {
        if self.url.is_empty() {
            return Ok(None);
        }

        if let Some(channel) = self.channel.read().await.clone() {
            return Ok(Some(Self::client_from_channel(channel)));
        }

        let mut guard = self.channel.write().await;
        if let Some(channel) = guard.clone() {
            return Ok(Some(Self::client_from_channel(channel)));
        }

        let channel = Channel::from_shared(self.url.clone())?.connect().await?;
        *guard = Some(channel.clone());

        let client = Self::client_from_channel(channel);

        Ok(Some(client))
    }

    pub async fn invalidate(&self) {
        let mut guard = self.channel.write().await;
        *guard = None;
    }

    fn client_from_channel(channel: Channel) -> CallbackClient {
        #[cfg(feature = "opentelemetry")]
        {
            LivestreamCallbackClient::with_interceptor(channel, TraceContextInterceptor::default())
        }

        #[cfg(not(feature = "opentelemetry"))]
        {
            LivestreamCallbackClient::new(channel)
        }
    }
}

impl Display for GrpcClientFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("GrpcClientFactory {{ {} }}", &self.url))
    }
}
