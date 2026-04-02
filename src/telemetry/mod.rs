#[cfg(feature = "opentelemetry")]
mod init;

pub mod metrics;

#[cfg(feature = "opentelemetry")]
#[allow(unused_imports)]
pub use init::{OtelGuard, setup_telemetry};

#[cfg(not(feature = "opentelemetry"))]
pub struct OtelGuard;

#[cfg(not(feature = "opentelemetry"))]
impl OtelGuard {
    pub fn shutdown(&self) {}
}

#[cfg(not(feature = "opentelemetry"))]
pub fn setup_telemetry() -> anyhow::Result<Option<OtelGuard>> {
    use tracing_subscriber::{
        EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt,
    };

    let fmt_layer = fmt::layer()
        .compact()
        .with_target(false)
        .with_filter(EnvFilter::from_default_env());

    tracing_subscriber::registry().with(fmt_layer).init();

    Ok(None)
}
