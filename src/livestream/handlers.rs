use log::{debug, info, warn};
use tokio::fs;
use tokio_stream::StreamExt;

use super::events::*;
use crate::services::{MinioClient, RedisClient};

pub(super) async fn stream_connected_handler(rx: StreamConnectedRx, redis: RedisClient) {
    let mut stream = StreamConnectedStream::new(rx);

    while let Some(Ok(connected)) = stream.next().await {
        info!("Received stream connected event: {:?}", connected);

        let result = redis
            .publish_stream_event(connected.live_id(), "connected")
            .await;
        if let Err(e) = result {
            warn!("Failed to publish stream event: {}", e);
        }
    }
}

pub(super) async fn stream_terminate_handler(rx: StreamTerminateRx, redis: RedisClient) {
    let mut stream = StreamTerminateStream::new(rx);

    while let Some(Ok(connected)) = stream.next().await {
        info!("Received stream terminate event: {:?}", connected);

        let result = redis
            .publish_stream_event(connected.live_id(), "terminate")
            .await;
        if let Err(e) = result {
            warn!("Failed to publish stream event: {}", e);
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
