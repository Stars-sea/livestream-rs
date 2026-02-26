use std::path::PathBuf;
use std::sync::Arc;

use tokio::fs;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{info, warn};

use super::events::*;
use super::grpc::livestream_callback_client::LivestreamCallbackClient;
use super::grpc::*;
use crate::ingest::puller::StreamPullerFactory;
use crate::services::MinioClient;

pub(super) async fn stream_message_handler(
    rx: mpsc::UnboundedReceiver<StreamMessage>,
    grpc_callback: String,
    minio: MinioClient,
    factory: Arc<StreamPullerFactory>,
) {
    let mut stream = UnboundedReceiverStream::new(rx);

    while let Some(msg) = stream.next().await {
        info!("Received stream message event: {:?}", msg);

        match msg {
            StreamMessage::SegmentComplete { live_id, path } => {
                segment_complete_handler(live_id, path, &minio).await.ok();
            }
            StreamMessage::StreamStarted { live_id } => {
                stream_started_handler(live_id, &grpc_callback).await;
            }
            StreamMessage::StreamStopped { live_id, error } => {
                stream_stopped_handler(live_id, error, &grpc_callback, &factory).await;
            }
        }
    }
}

async fn stream_started_handler(live_id: String, grpc_callback: &String) {
    if grpc_callback.is_empty() {
        return;
    }
    if let Ok(mut client) = LivestreamCallbackClient::connect(grpc_callback.clone()).await {
        let req = NotifyStartedRequest { live_id };
        if let Err(e) = client.notify_stream_started(req).await {
            warn!("Failed to notify stream started: {}", e);
        }
    } else {
        warn!("Failed to connect to gRPC callback at {}", grpc_callback);
    }
}

async fn stream_stopped_handler(
    live_id: String,
    error_message: Option<String>,
    grpc_callback: &String,
    factory: &Arc<StreamPullerFactory>,
) {
    factory.remove_signal(&live_id).await;

    if grpc_callback.is_empty() {
        return;
    }
    if let Ok(mut client) = LivestreamCallbackClient::connect(grpc_callback.clone()).await {
        let req = NotifyStoppedRequest {
            live_id,
            error_message,
        };
        if let Err(e) = client.notify_stream_stopped(req).await {
            warn!("Failed to notify stream stopped: {}", e);
        }
    } else {
        warn!("Failed to connect to gRPC callback at {}", grpc_callback);
    }
}

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
        warn!("Upload failed for {}: {:?}", path.display(), e);
        anyhow::bail!("Failed to upload file {}: {:?}", path.display(), e);
    }

    if fs::remove_file(&path).await.is_err() {
        warn!("Failed to remove file {}", path.display());
    }
    Ok(())
}
