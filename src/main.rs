use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::{JoinError, JoinHandle};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::channel::{MpscRx, MpscTx};
use crate::config::{AppConfig, MinioConfig, PersistenceConfig, QueueConfig};
use crate::infra::PersistenceClient;
use crate::infra::media::packet::FlvTag;
use crate::pipeline::handler::SegmentPersistenceHandler;
use crate::pipeline::{PipeBus, UnifiedPipeFactory};
use crate::transport::abstraction::IngestPacket;
use crate::transport::flv::FlvEgressHub;
use crate::transport::grpc::GrpcServer;
use crate::transport::http_flv::HttpFlvServer;
use crate::transport::{TransportController, TransportServer};

mod abstraction;
mod channel;
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
    let cancel_token = CancellationToken::new();
    let flv_egress_hub = Arc::new(FlvEgressHub::new());
    let (tx, rx) = channel::mpsc("flv_egress", None, config.queue.rtmp_forward);

    spawn_persistence_handler(
        config
            .storage
            .minio
            .clone()
            .expect("Minio config is required"),
    )
    .await?;

    let packet_bus = build_pipe_bus(&config.storage.persistence, &config.queue, tx);

    let (controller, controller_task) = build_transport_server(
        config,
        packet_bus,
        rx,
        flv_egress_hub.clone(),
        cancel_token.clone(),
    )
    .await?;

    let grpc_task = spawn_grpc_server(config, controller, cancel_token.child_token());
    let http_flv_task = spawn_http_flv_server(config, flv_egress_hub, cancel_token.child_token())?;

    wait_for_tasks(controller_task, grpc_task, http_flv_task, cancel_token).await;

    if let Some(guard) = otel_guard {
        guard.shutdown();
    }

    Ok(())
}

async fn spawn_persistence_handler(minio: MinioConfig) -> Result<()> {
    let minio_client = PersistenceClient::create(minio).await?;
    SegmentPersistenceHandler::spawn(minio_client);
    Ok(())
}

fn build_pipe_bus(
    persistence_config: &PersistenceConfig,
    queue_config: &QueueConfig,
    flv_tag_tx: MpscTx<IngestPacket<FlvTag>>,
) -> PipeBus {
    let segment_duration = Duration::from_secs(persistence_config.duration.max(1) as u64);
    let segment_cache_dir = persistence_config.cache_dir.trim().to_string();

    let factory = Arc::new(UnifiedPipeFactory::new(
        segment_duration,
        segment_cache_dir,
        queue_config.flv_relay,
        flv_tag_tx,
    ));
    let bus = PipeBus::new();
    bus.spawn_session_listener(factory);
    bus
}

async fn build_transport_server(
    config: &AppConfig,
    packet_bus: PipeBus,
    rx: MpscRx<IngestPacket<FlvTag>>,
    flv_egress_hub: Arc<FlvEgressHub>,
    cancel_token: CancellationToken,
) -> Result<(Arc<TransportController>, JoinHandle<Result<()>>)> {
    let server = TransportServer::new(
        config.transport.rtmp.clone(),
        config.transport.srt.clone(),
        config.queue.clone(),
        rx,
        flv_egress_hub,
        packet_bus,
        cancel_token.child_token(),
    );

    let (controller, task) = server.spawn_task().await?;
    Ok((Arc::new(controller), task))
}

fn spawn_grpc_server(
    config: &config::AppConfig,
    controller: Arc<TransportController>,
    cancel_token: CancellationToken,
) -> JoinHandle<Result<()>> {
    let grpc_server = GrpcServer::new(
        config.services.grpc.clone(),
        config.transport.rtmp.clone(),
        config.services.http_flv.clone(),
        controller,
    );
    tokio::spawn(async move { grpc_server.serve(cancel_token).await })
}

fn spawn_http_flv_server(
    config: &config::AppConfig,
    flv_egress_hub: Arc<FlvEgressHub>,
    cancel_token: CancellationToken,
) -> Result<Option<JoinHandle<Result<()>>>> {
    if !config.services.http_flv.enabled {
        return Ok(None);
    }

    let http_flv_config = config.services.http_flv.clone();
    Ok(Some(tokio::spawn(async move {
        let server = HttpFlvServer::create(http_flv_config, flv_egress_hub, cancel_token).await?;
        server.run().await
    })))
}

async fn wait_for_tasks(
    mut controller_task: JoinHandle<Result<()>>,
    mut grpc_task: JoinHandle<Result<()>>,
    http_flv_task: Option<JoinHandle<Result<()>>>,
    cancel_token: CancellationToken,
) {
    match http_flv_task {
        Some(mut http_flv_task) => {
            tokio::select! {
                transport_result = &mut controller_task => {
                    log_server_task_result("transport", transport_result);
                    cancel_token.cancel();
                    await_server_task_with_timeout("gRPC", &mut grpc_task).await;
                    await_server_task_with_timeout("HTTP-FLV", &mut http_flv_task).await;
                }
                grpc_result = &mut grpc_task => {
                    log_server_task_result("gRPC", grpc_result);
                    cancel_token.cancel();
                    await_server_task_with_timeout("transport", &mut controller_task).await;
                    await_server_task_with_timeout("HTTP-FLV", &mut http_flv_task).await;
                }
                http_flv_result = &mut http_flv_task => {
                    log_server_task_result("HTTP-FLV", http_flv_result);
                    cancel_token.cancel();
                    await_server_task_with_timeout("transport", &mut controller_task).await;
                    await_server_task_with_timeout("gRPC", &mut grpc_task).await;
                }
            }
        }
        None => {
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
        }
    }
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
