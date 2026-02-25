use std::path::PathBuf;

use log::{debug, info, warn};
use tokio::fs;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use super::events::*;
use super::grpc::livestream_callback_client::LivestreamCallbackClient;
use super::grpc::*;
use crate::services::MinioClient;

pub(super) async fn stream_message_handler(
    rx: broadcast::Receiver<StreamMessage>,
    grpc_callback: String,
    minio: MinioClient,
) {
    let mut stream = BroadcastStream::new(rx);

    while let Some(Ok(msg)) = stream.next().await {
        info!("Received stream message event: {:?}", msg);

        match msg {
            StreamMessage::SegmentComplete {
                live_id,
                segment_id,
                path,
            } => {
                if let Err(e) = segment_complete_handler(live_id, segment_id, path, &minio).await {
                    warn!("Failed to handle segment complete event: {}", e);
                }
            }
            StreamMessage::StreamStarted { live_id } => {
                stream_started_handler(live_id, &grpc_callback).await;
            }
            StreamMessage::StreamStopped {
                live_id,
                error,
                path,
            } => {
                stream_stopped_handler(live_id, error, path, &grpc_callback).await;
            }
        }
    }
}

async fn stream_started_handler(live_id: String, grpc_callback: &String) {
    info!("Received stream connected event: {:?}", live_id);

    if let Ok(mut client) = LivestreamCallbackClient::connect(grpc_callback.clone()).await {
        let req = NotifyConnectedRequest { live_id };
        if let Err(e) = client.notify_livestream_connected(req).await {
            warn!("Failed to notify stream connected: {}", e);
        }
    } else {
        warn!("Failed to connect to gRPC callback at {}", grpc_callback);
    }
}

async fn stream_stopped_handler(
    live_id: String,
    error_message: Option<String>,
    path: PathBuf,
    grpc_callback: &String,
) {
    info!("Received stream terminate event: {:?}", live_id);

    if let Ok(mut client) = LivestreamCallbackClient::connect(grpc_callback.clone()).await {
        let req = NotifyTerminateRequest {
            live_id,
            error_message,
        };
        if let Err(e) = client.notify_livestream_terminate(req).await {
            warn!("Failed to notify stream terminated: {}", e);
        }
    } else {
        warn!("Failed to connect to gRPC callback at {}", grpc_callback);
    }

    // Clean up any remaining files in the cache directory
    if fs::try_exists(&path).await.unwrap_or(false) {
        if let Err(e) = fs::remove_dir_all(&path).await {
            warn!("Failed to remove cache directory {}: {}", path.display(), e);
        }
    }
}

async fn segment_complete_handler(
    live_id: String,
    segment_id: String,
    path: PathBuf,
    minio: &MinioClient,
) -> anyhow::Result<()> {
    info!("Uploading file {}", path.display());

    let storage_key = format!("{}/{}", live_id, segment_id);
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

    debug!("Remove file {}", path.display());
    if fs::remove_file(&path).await.is_err() {
        warn!("Failed to remove file {}", path.display());
    }
    Ok(())
}
