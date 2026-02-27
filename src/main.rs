use std::sync::Arc;
use std::sync::OnceLock;

use anyhow::Result;
use opentelemetry::global;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{LogExporter, MetricExporter, SpanExporter};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{RandomIdGenerator, Sampler, SdkTracerProvider};
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::ingest::{GrpcServerFactory, LivestreamService, StreamManager};
use crate::publish::RtmpServer;

mod core;
mod ingest;
mod publish;
mod services;
mod settings;

fn resource() -> Resource {
    static RESOURCE: OnceLock<Resource> = OnceLock::new();

    RESOURCE
        .get_or_init(|| {
            let resource_name = match std::env::var("OTEL_SERVICE_NAME") {
                Ok(name) => name,
                Err(_) => "livestream-rs".to_string(),
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

#[tokio::main]
async fn main() -> Result<()> {
    let logger_provider = init_logs()?;

    let otel_layer = OpenTelemetryTracingBridge::new(&logger_provider);

    let filter_otel = EnvFilter::new("info")
        .add_directive("hyper=off".parse().unwrap())
        .add_directive("tonic=off".parse().unwrap())
        .add_directive("h2=off".parse().unwrap())
        .add_directive("reqwest=off".parse().unwrap());
    let otel_layer = otel_layer.with_filter(filter_otel);

    let filter_fmt = EnvFilter::new("info");
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_thread_names(true)
        .with_filter(filter_fmt);

    tracing_subscriber::registry()
        .with(otel_layer)
        .with(fmt_layer)
        .init();

    let tracer_provider = init_tracer()?;
    global::set_tracer_provider(tracer_provider.clone());

    let meter_provider = init_metrics()?;
    global::set_meter_provider(meter_provider.clone());

    info!(
        version = env!("CARGO_PKG_VERSION"),
        "Starting LiveStream server"
    );

    let settings = match settings::Settings::new() {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "Failed to load configuration");
            return Err(e);
        }
    };

    // core::set_log_level(Level::Trace);
    core::set_log_quiet();
    core::init();

    let (tx, rx) = mpsc::unbounded_channel();
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    let minio_client = services::MinioClient::create(settings.minio.unwrap()).await?;
    let manager = Arc::new(StreamManager::new(
        settings.ingest.clone(),
        minio_client,
        tx,
    ));

    // Start RTMP server
    let server = RtmpServer::new(settings.publish, manager.clone());

    let shutdown_rx = shutdown_tx.subscribe();
    tokio::spawn(async move {
        if let Err(e) = server.start(rx, shutdown_rx).await {
            tracing::error!(error = %e, "RTMP server failed");
        }
    });

    // Start GRPC Server & SRT Stream Puller
    let grpc_future = GrpcServerFactory::new()
        .with_service(LivestreamService::new(manager.clone()))
        .with_manager(manager)
        .with_config(settings.ingest)
        .serve();

    // Wait for gRPC server (which listens for Ctrl+C)
    if let Err(e) = grpc_future.await {
        error!(error = %e, "gRPC server error");
    }

    // Signal other components to shutdown
    let _ = shutdown_tx.send(());

    let _ = logger_provider.shutdown();
    let _ = tracer_provider.shutdown();
    let _ = meter_provider.shutdown();

    Ok(())
}
