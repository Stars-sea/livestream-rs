use std::sync::OnceLock;

use anyhow::Result;
use opentelemetry::global;
use opentelemetry::trace::TracerProvider;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{LogExporter, MetricExporter, SpanExporter};
use opentelemetry_sdk::{
    Resource,
    logs::SdkLoggerProvider,
    metrics::SdkMeterProvider,
    propagation::TraceContextPropagator,
    trace::{RandomIdGenerator, Sampler, SdkTracerProvider},
};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

const OTEL_SERVICE_NAME: &str = "OTEL_SERVICE_NAME";

pub fn otlp_enabled() -> bool {
    std::env::vars_os().any(|(key, _)| key.to_string_lossy().starts_with("OTEL_"))
}

pub fn resource() -> Resource {
    static RESOURCE: OnceLock<Resource> = OnceLock::new();

    RESOURCE
        .get_or_init(|| {
            let resource_name = match std::env::var(OTEL_SERVICE_NAME) {
                Ok(name) => name,
                Err(_) => env!("CARGO_PKG_NAME").to_string(),
            };
            Resource::builder().with_service_name(resource_name).build()
        })
        .clone()
}

fn init_logs() -> Result<SdkLoggerProvider> {
    let exporter = LogExporter::builder().with_tonic().build()?;

    let logger_provider = SdkLoggerProvider::builder()
        .with_resource(resource())
        .with_batch_exporter(exporter)
        .build();

    Ok(logger_provider)
}

fn init_tracer() -> Result<SdkTracerProvider> {
    global::set_text_map_propagator(TraceContextPropagator::new());

    let exporter = SpanExporter::builder().with_tonic().build()?;

    let provider = SdkTracerProvider::builder()
        .with_resource(resource())
        .with_batch_exporter(exporter)
        .with_sampler(Sampler::AlwaysOn)
        .with_id_generator(RandomIdGenerator::default())
        .build();

    Ok(provider)
}

fn init_metrics() -> Result<SdkMeterProvider> {
    let exporter = MetricExporter::builder().with_tonic().build()?;

    let provider = SdkMeterProvider::builder()
        .with_periodic_exporter(exporter)
        .with_resource(resource())
        .build();

    Ok(provider)
}

pub fn init_otlp() -> Result<(SdkLoggerProvider, SdkTracerProvider, SdkMeterProvider)> {
    if !otlp_enabled() {
        anyhow::bail!("OpenTelemetry disabled: no OTEL_* environment variables found")
    }

    let logger_provider = init_logs()?;
    let tracer_provider = init_tracer()?;
    let meter_provider = init_metrics()?;

    Ok((logger_provider, tracer_provider, meter_provider))
}

pub struct OtelGuard {
    logger: SdkLoggerProvider,
    tracer: SdkTracerProvider,
    meter: SdkMeterProvider,
}

impl OtelGuard {
    pub fn shutdown(&self) {
        let _ = self.logger.shutdown();
        let _ = self.tracer.shutdown();
        let _ = self.meter.shutdown();
    }
}

pub fn setup_telemetry() -> Result<Option<OtelGuard>> {
    if !otlp_enabled() {
        let fmt_layer = fmt::layer()
            .compact()
            .with_target(false)
            .with_filter(EnvFilter::from_default_env());

        tracing_subscriber::registry().with(fmt_layer).init();
        return Ok(None);
    }

    let (logger_provider, tracer_provider, meter_provider) = init_otlp()?;
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

    Ok(Some(OtelGuard {
        logger: logger_provider,
        tracer: tracer_provider,
        meter: meter_provider,
    }))
}
