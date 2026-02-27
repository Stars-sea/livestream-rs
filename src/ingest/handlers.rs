use std::path::PathBuf;
use std::sync::Arc;

use tokio::fs;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{info, instrument, warn};

use super::events::*;
use super::grpc::*;
use crate::ingest::factory::GrpcClientFactory;
use crate::ingest::puller::StreamPullerFactory;
use crate::services::MinioClient;

#[instrument(skip(rx, client_factory, minio, factory))]
pub(super) async fn stream_message_handler(
    rx: mpsc::UnboundedReceiver<StreamMessage>,
    client_factory: GrpcClientFactory,
    minio: MinioClient,
    factory: Arc<StreamPullerFactory>,
) {
    let mut stream = UnboundedReceiverStream::new(rx);

    while let Some(msg) = stream.next().await {
        info!(msg = ?msg, "Received stream message event");

        match msg {
            StreamMessage::SegmentComplete { live_id, path } => {
                segment_complete_handler(live_id, path, &minio).await.ok();
            }
            StreamMessage::StreamStarted { live_id } => {
                stream_started_handler(live_id, &client_factory).await;
            }
            StreamMessage::StreamStopped { live_id, error } => {
                stream_stopped_handler(live_id, error, &client_factory, &factory).await;
            }
        }
    }
}

#[instrument(skip(client_factory), fields(live_id = %live_id))]
async fn stream_started_handler(live_id: String, client_factory: &GrpcClientFactory) {
    if let Ok(Some(mut client)) = client_factory.build().await {
        let req = NotifyStartedRequest { live_id };
        if let Err(e) = client.notify_stream_started(req).await {
            warn!(error = %e, "Failed to notify stream started");
        }
    } else {
        warn!(client = %client_factory, "Failed to connect to gRPC callback");
    }
}

#[instrument(skip(client_factory, factory), fields(live_id = %live_id))]
async fn stream_stopped_handler(
    live_id: String,
    error_message: Option<String>,
    client_factory: &GrpcClientFactory,
    factory: &Arc<StreamPullerFactory>,
) {
    factory.remove_signal(&live_id).await;

    if let Ok(Some(mut client)) = client_factory.build().await {
        let req = NotifyStoppedRequest {
            live_id,
            error_message,
        };
        if let Err(e) = client.notify_stream_stopped(req).await {
            warn!(error = %e, "Failed to notify stream stopped");
        }
    } else {
        warn!(client = %client_factory, "Failed to connect to gRPC callback");
    }
}

#[instrument(skip(minio), fields(live_id = %live_id, path = %path.display()))]
async fn segment_complete_handler(
    live_id: String,
    path: PathBuf,
    minio: &MinioClient,
) -> anyhow::Result<()> {
    let filename = path
        .file_name()
        .ok_or(anyhow::anyhow!("Failed to get file name"))?;
    let storage_key = format!("{}/{}", live_id, filename.to_string_lossy());

    let upload_resp = minio
        .upload_file(
            storage_key.as_str(),
            fs::canonicalize(&path).await?.as_path(),
        )
        .await;

    if let Err(e) = upload_resp {
        warn!(path = %path.display(), error = ?e, "Upload failed");
        anyhow::bail!("Failed to upload file {}: {:?}", path.display(), e);
    }

    if fs::remove_file(&path).await.is_err() {
        warn!("Failed to remove file {}", path.display());
    }
    Ok(())
}
