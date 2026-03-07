use anyhow::Result;
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, Layer, fmt};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[cfg(feature = "opentelemetry")]
use opentelemetry::global;
#[cfg(feature = "opentelemetry")]
use opentelemetry::trace::TracerProvider;
#[cfg(feature = "opentelemetry")]
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
#[cfg(feature = "opentelemetry")]
use tracing_opentelemetry::OpenTelemetryLayer;

use crate::api::AppServer;

mod api;
mod config;
mod egress;
mod infra;
mod ingest;
mod media;
mod telemetry;
mod transport;

#[tokio::main]
async fn main() -> Result<()> {
    #[cfg(feature = "opentelemetry")]
    let mut providers = None;

    #[cfg(feature = "opentelemetry")]
    if telemetry::otlp_enabled() {
        let (logger_provider, tracer_provider, meter_provider) = telemetry::init_otlp()?;
        let tracer = tracer_provider.tracer("livestream-rs");

        let fmt_layer = fmt::layer()
            .compact()
            .with_target(false)
            .with_filter(EnvFilter::from_default_env());

        let otel_layer = OpenTelemetryTracingBridge::new(&logger_provider)
            .with_filter(EnvFilter::from_default_env());

        let otel_trace_layer =
            OpenTelemetryLayer::new(tracer).with_filter(EnvFilter::from_default_env());

        tracing_subscriber::registry()
            .with(otel_layer)
            .with(otel_trace_layer)
            .with(fmt_layer)
            .init();

        global::set_tracer_provider(tracer_provider.clone());
        global::set_meter_provider(meter_provider.clone());

        providers = Some((logger_provider, tracer_provider, meter_provider));
    } else {
        let fmt_layer = fmt::layer()
            .compact()
            .with_target(false)
            .with_filter(EnvFilter::from_default_env());

        tracing_subscriber::registry().with(fmt_layer).init();
    }

    #[cfg(not(feature = "opentelemetry"))]
    {
        let fmt_layer = fmt::layer()
            .compact()
            .with_target(false)
            .with_filter(EnvFilter::from_default_env());

        tracing_subscriber::registry().with(fmt_layer).init();
    }

    info!(
        version = env!("CARGO_PKG_VERSION"),
        "Starting LiveStream server"
    );

    media::init();

    if let Err(e) = AppServer::run().await {
        error!(error = %e, "Server runtime error");
    }

    #[cfg(feature = "opentelemetry")]
    if let Some((logger_provider, tracer_provider, meter_provider)) = providers {
        let _ = logger_provider.shutdown();
        let _ = tracer_provider.shutdown();
        let _ = meter_provider.shutdown();
    }

    Ok(())
}
