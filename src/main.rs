use std::sync::Arc;

use anyhow::Result;
use opentelemetry::{global, trace::TracerProvider as _};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::ingest::{GrpcServerFactory, LivestreamService, StreamManager};
use crate::publish::RtmpServer;

mod core;
mod ingest;
mod publish;
mod services;
mod settings;

#[tokio::main]
async fn main() -> Result<()> {
    global::set_text_map_propagator(TraceContextPropagator::new());

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .build()?;

    let tracer_provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::AlwaysOn)))
        .with_resource(
            Resource::builder()
                .with_service_name("livestream-rs")
                .build(),
        )
        .build();

    let tracer = tracer_provider.tracer("livestream-rs");
    global::set_tracer_provider(tracer_provider.clone());

    let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

    let filter = tracing_subscriber::EnvFilter::new(
        std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
    );

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(telemetry)
        .init();

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

    let _ = tracer_provider.shutdown();

    Ok(())
}
