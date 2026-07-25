use std::future::Future;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

#[cfg(feature = "opentelemetry")]
use opentelemetry::propagation::Extractor;
#[cfg(feature = "opentelemetry")]
use opentelemetry::{Context as OtelContext, global};
use tonic::codegen::{Service, http};

#[cfg(feature = "opentelemetry")]
struct HeaderMapExtractor<'a>(&'a http::HeaderMap);

#[cfg(feature = "opentelemetry")]
impl Extractor for HeaderMapExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|key| key.as_str()).collect()
    }
}

#[cfg(feature = "opentelemetry")]
fn extract_remote_context(headers: &http::HeaderMap) -> OtelContext {
    global::get_text_map_propagator(|prop| {
        let extractor = HeaderMapExtractor(headers);
        prop.extract(&extractor)
    })
}

#[cfg(feature = "opentelemetry")]
#[derive(Clone)]
pub(super) struct OtelContextPropagationService<S> {
    inner: S,
}

#[cfg(feature = "opentelemetry")]
impl<S> OtelContextPropagationService<S> {
    pub(super) fn new(inner: S) -> Self {
        Self { inner }
    }
}

#[cfg(feature = "opentelemetry")]
impl<S, B> Service<http::Request<B>> for OtelContextPropagationService<S>
where
    S: Service<http::Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = OtelContextPropagationFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        let parent_cx = extract_remote_context(request.headers());
        let future = Box::pin(self.inner.call(request));

        OtelContextPropagationFuture { future, parent_cx }
    }
}

#[cfg(feature = "opentelemetry")]
impl<S> tonic::server::NamedService for OtelContextPropagationService<S>
where
    S: tonic::server::NamedService,
{
    const NAME: &'static str = S::NAME;
}

#[cfg(feature = "opentelemetry")]
pub(super) struct OtelContextPropagationFuture<F> {
    future: Pin<Box<F>>,
    parent_cx: OtelContext,
}

#[cfg(feature = "opentelemetry")]
impl<F> Future for OtelContextPropagationFuture<F>
where
    F: Future,
{
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let _guard = self.parent_cx.clone().attach();
        self.future.as_mut().poll(cx)
    }
}
