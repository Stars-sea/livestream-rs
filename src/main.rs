use std::sync::Arc;

use anyhow::Result;
use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{EnvFilter, Layer, fmt};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::ingest::{GrpcServerFactory, LivestreamService, StreamManager};
use crate::publish::RtmpServer;

mod core;
mod ingest;
mod init_otlp;
mod publish;
mod services;
mod settings;

#[tokio::main]
async fn main() -> Result<()> {
    let (logger_provider, tracer_provider, meter_provider) = init_otlp::init_otlp()?;
    let tracer = tracer_provider.tracer("livestream-rs");

    let otel_layer = OpenTelemetryTracingBridge::new(&logger_provider)
        .with_filter(EnvFilter::from_default_env());

    let otel_trace_layer =
        OpenTelemetryLayer::new(tracer).with_filter(EnvFilter::from_default_env());

    let fmt_layer = fmt::layer()
        .with_target(false)
        .with_thread_names(false)
        .with_file(true)
        .with_filter(EnvFilter::from_default_env());

    tracing_subscriber::registry()
        .with(otel_layer)
        .with(otel_trace_layer)
        .with(fmt_layer)
        .init();

    global::set_tracer_provider(tracer_provider.clone());

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
