use log::{debug, info, warn};
use tokio::fs;
use tokio_stream::StreamExt;

use super::events::*;
use super::grpc::livestream_callback_client::LivestreamCallbackClient;
use super::grpc::*;
use crate::services::MinioClient;

pub(super) async fn stream_connected_handler(rx: StreamConnectedRx, grpc_callback: String) {
    let mut stream = StreamConnectedStream::new(rx);

    while let Some(Ok(connected)) = stream.next().await {
        info!("Received stream connected event: {:?}", connected);

        if let Ok(mut client) = LivestreamCallbackClient::connect(grpc_callback.clone()).await {
            let req: NotifyConnectedRequest = connected.into();
            if let Err(e) = client.notify_livestream_connected(req).await {
                warn!("Failed to notify stream connected: {}", e);
            }
        } else {
            warn!("Failed to connect to gRPC callback at {}", grpc_callback);
        }
    }
}

pub(super) async fn stream_terminate_handler(rx: StreamTerminateRx, grpc_callback: String) {
    let mut stream = StreamTerminateStream::new(rx);

    while let Some(Ok(connected)) = stream.next().await {
        info!("Received stream terminate event: {:?}", connected);

        if let Ok(mut client) = LivestreamCallbackClient::connect(grpc_callback.clone()).await {
            let req: NotifyTerminateRequest = connected.into();
            if let Err(e) = client.notify_livestream_terminate(req).await {
                warn!("Failed to notify stream terminated: {}", e);
            }
        } else {
            warn!("Failed to connect to gRPC callback at {}", grpc_callback);
        }
    }
}

pub(super) async fn segment_complete_handler(
    rx: SegmentCompleteRx,
    minio: MinioClient,
) -> anyhow::Result<()> {
    let mut stream = SegmentCompleteStream::new(rx);

    while let Some(Ok(complete_info)) = stream.next().await {
        let path = complete_info.path();
        info!("Uploading file {}", path.display());

        let storage_key = format!("{}/{}", complete_info.live_id(), complete_info.segment_id());
        let upload_resp = minio
            .upload_file(
                storage_key.as_str(),
                fs::canonicalize(&path).await?.as_path(),
            )
            .await;

        if let Err(e) = upload_resp {
            warn!("Upload failed for {}: {:?}", path.display(), e);
            continue;
        }

        debug!("Remove file {}", path.display());
        if fs::remove_file(&path).await.is_err() {
            warn!("Failed to remove file {}", path.display());
        }
    }
    Ok(())
}

impl From<OnStreamConnected> for NotifyConnectedRequest {
    fn from(event: OnStreamConnected) -> Self {
        Self {
            live_id: event.live_id().to_string(),
        }
    }
}

impl From<OnStreamTerminate> for NotifyTerminateRequest {
    fn from(event: OnStreamTerminate) -> Self {
        Self {
            live_id: event.live_id().to_string(),
            error_message: event.error().clone(),
        }
    }
}
