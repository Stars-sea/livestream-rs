use std::sync::OnceLock;

use anyhow::Result;
use opentelemetry::global;
use opentelemetry_otlp::{LogExporter, MetricExporter, SpanExporter};
use opentelemetry_sdk::{
    Resource,
    logs::SdkLoggerProvider,
    metrics::SdkMeterProvider,
    propagation::TraceContextPropagator,
    trace::{RandomIdGenerator, Sampler, SdkTracerProvider},
};

const OTEL_SERVICE_NAME: &str = "OTEL_SERVICE_NAME";

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
    let exporter = MetricExporter::builder()
        .with_tonic()
        .build()
        .expect("Failed to create metric exporter");

    let provider = SdkMeterProvider::builder()
        .with_periodic_exporter(exporter)
        .with_resource(resource())
        .build();

    Ok(provider)
}

pub fn init_otlp() -> Result<(SdkLoggerProvider, SdkTracerProvider, SdkMeterProvider)> {
    let logger_provider = init_logs()?;
    let tracer_provider = init_tracer()?;
    let meter_provider = init_metrics()?;

    Ok((logger_provider, tracer_provider, meter_provider))
}
