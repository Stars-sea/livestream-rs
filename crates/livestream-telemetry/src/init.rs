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

/// OTLP exporter endpoint environment variables. Setting any of these is an
/// explicit request to export telemetry over OTLP; signals without their own
/// endpoint fall back to `OTEL_EXPORTER_OTLP_ENDPOINT` and then to the SDK
/// default (`localhost:4317`).
const OTLP_ENDPOINT_ENV_VARS: &[&str] = &[
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
    "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
];

/// Whether the user explicitly selected OTLP export without configuring an
/// endpoint: per-signal `OTEL_*_EXPORTER=otlp` or a protocol override.
/// These imply the default endpoint fallback should apply.
fn otlp_exporter_selected() -> bool {
    [
        "OTEL_TRACES_EXPORTER",
        "OTEL_METRICS_EXPORTER",
        "OTEL_LOGS_EXPORTER",
    ]
    .iter()
    .any(|key| {
        std::env::var(key)
            .map(|value| value.trim().eq_ignore_ascii_case("otlp"))
            .unwrap_or(false)
    }) || std::env::var_os("OTEL_EXPORTER_OTLP_PROTOCOL").is_some()
}

/// True only when the environment expresses an explicit OTLP export intent.
/// Unrelated `OTEL_*` variables (e.g. just `OTEL_SERVICE_NAME`) must not
/// enable export, which would otherwise spam a non-existent collector.
pub fn otlp_enabled() -> bool {
    OTLP_ENDPOINT_ENV_VARS
        .iter()
        .any(|key| std::env::var_os(key).is_some())
        || otlp_exporter_selected()
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
        anyhow::bail!("OpenTelemetry disabled: no OTLP exporter configured")
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

impl Drop for OtelGuard {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Install the console-only fmt subscriber used when OTLP export is disabled
/// or failed to initialize.
fn install_console_logging() {
    let fmt_layer = fmt::layer()
        .compact()
        .with_target(false)
        .with_filter(EnvFilter::from_default_env());
    tracing_subscriber::registry().with(fmt_layer).init();
}

pub fn setup_telemetry() -> Result<Option<OtelGuard>> {
    if !otlp_enabled() {
        install_console_logging();
        return Ok(None);
    }

    // Telemetry is best-effort: a misconfigured exporter (e.g. a malformed
    // OTEL_EXPORTER_OTLP_ENDPOINT) must not prevent the service from
    // starting. Degrade to console logging instead of propagating the error.
    let (logger_provider, tracer_provider, meter_provider) = match init_otlp() {
        Ok(providers) => providers,
        Err(error) => {
            install_console_logging();
            tracing::warn!(
                error = %error,
                "OpenTelemetry export initialization failed; falling back to console logging"
            );
            return Ok(None);
        }
    };
    let tracer = tracer_provider.tracer("livestream");

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
