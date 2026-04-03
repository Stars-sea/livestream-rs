use anyhow::{Context, Result};
use crossfire::mpsc;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::{JoinError, JoinHandle};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::infra::MinioClient;
use crate::pipeline::handler::SegmentPersistenceHandler;
use crate::pipeline::{PipeBus, UnifiedPipeFactory};
use crate::transport::TransportServer;
use crate::transport::grpc::GrpcServer;

mod abstraction;
mod config;
mod dispatcher;
mod infra;
mod pipeline;
mod telemetry;
mod transport;

const SERVER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<()> {
    let otel_guard = telemetry::setup_telemetry()?;

    info!(
        version = env!("CARGO_PKG_VERSION"),
        "Starting LiveStream server"
    );

    infra::media::init();

    let config = config::load_config();
    let segment_duration = Duration::from_secs(config.srt.duration.max(1) as u64);

    let minio = config
        .minio
        .clone()
        .context("MinIO configuration is missing")?;
    let minio_client = MinioClient::create(minio).await?;
    SegmentPersistenceHandler::spawn(minio_client);

    let cancel_token = CancellationToken::new();
    let (rtmp_tag_tx, rtmp_tag_rx) = mpsc::unbounded_async();

    let factory = Arc::new(UnifiedPipeFactory::new(segment_duration, rtmp_tag_tx));
    let packet_bus = PipeBus::new();
    packet_bus.spawn_session_listener(factory);

    let transport_server = TransportServer::new(
        config.rtmp.clone(),
        config.srt.clone(),
        rtmp_tag_rx,
        packet_bus,
        cancel_token.child_token(),
    );

    let (controller, mut controller_task) = transport_server.spawn_task().await?;
    let controller = Arc::new(Mutex::new(controller));

    let grpc_server = GrpcServer::new(config.grpc.clone(), config.rtmp.clone(), controller.clone());

    let grpc_cancel = cancel_token.child_token();

    let mut grpc_task = tokio::spawn(async move { grpc_server.serve(grpc_cancel).await });

    tokio::select! {
        transport_result = &mut controller_task => {
            log_server_task_result("transport", transport_result);
            cancel_token.cancel();
            await_server_task_with_timeout("gRPC", &mut grpc_task).await;
        }
        grpc_result = &mut grpc_task => {
            log_server_task_result("gRPC", grpc_result);
            cancel_token.cancel();
            await_server_task_with_timeout("transport", &mut controller_task).await;
        }
    }

    if let Some(guard) = otel_guard {
        guard.shutdown();
    }

    Ok(())
}

async fn await_server_task_with_timeout(server_name: &str, task: &mut JoinHandle<Result<()>>) {
    match timeout(SERVER_SHUTDOWN_TIMEOUT, &mut *task).await {
        Ok(result) => log_server_task_result(server_name, result),
        Err(_) => {
            warn!(
                server = %server_name,
                timeout_secs = SERVER_SHUTDOWN_TIMEOUT.as_secs(),
                "Timed out waiting for server task shutdown; aborting task"
            );
            task.abort();
        }
    }
}

fn log_server_task_result(server_name: &str, result: std::result::Result<Result<()>, JoinError>) {
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            error!("Error running {} server: {}", server_name, e);
        }
        Err(e) => {
            error!("{} server task failed to join: {}", server_name, e);
        }
    }
}
