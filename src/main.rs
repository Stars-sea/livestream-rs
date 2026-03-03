use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{broadcast, mpsc};
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

use crate::ingest::{GrpcServerFactory, LivestreamService, StreamManager};
use crate::publish::RtmpServer;

mod core;
mod ingest;
mod otlp;
mod publish;
mod services;
mod settings;

#[tokio::main]
async fn main() -> Result<()> {
    #[cfg(feature = "opentelemetry")]
    let mut providers = None;

    #[cfg(feature = "opentelemetry")]
    if otlp::otlp_enabled() {
        let (logger_provider, tracer_provider, meter_provider) = otlp::init_otlp()?;
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

    core::init();

    let (tx, rx) = mpsc::unbounded_channel();
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    let minio_client = services::MinioClient::create_default().await?;
    let manager = Arc::new(StreamManager::new(minio_client, tx));

    // Start RTMP server
    let server = RtmpServer::new(manager.clone());

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
        .with_default_config()
        .serve();

    // Wait for gRPC server (which listens for Ctrl+C)
    if let Err(e) = grpc_future.await {
        error!(error = %e, "gRPC server error");
    }

    // Signal other components to shutdown
    let _ = shutdown_tx.send(());

    #[cfg(feature = "opentelemetry")]
    if let Some((logger_provider, tracer_provider, meter_provider)) = providers {
        let _ = logger_provider.shutdown();
        let _ = tracer_provider.shutdown();
        let _ = meter_provider.shutdown();
    }

    Ok(())
}
