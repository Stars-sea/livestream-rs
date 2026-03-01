use std::path::PathBuf;
use std::sync::Arc;

use tokio::fs;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{Instrument, Level, debug, warn};
use tracing::{Span, event};

use super::events::*;
use super::grpc::*;
use crate::ingest::factory::GrpcClientFactory;
use crate::ingest::puller::StreamPullerFactory;
use crate::services::MinioClient;

pub(super) async fn stream_message_handler(
    rx: mpsc::UnboundedReceiver<(StreamMessage, Span)>,
    client_factory: GrpcClientFactory,
    minio: MinioClient,
    factory: Arc<StreamPullerFactory>,
) {
    let mut stream = UnboundedReceiverStream::new(rx);

    while let Some((msg, span)) = stream.next().await {
        debug!(msg = ?msg, "Received stream message event");

        match msg {
            StreamMessage::SegmentComplete { live_id, path } => {
                segment_complete_handler(live_id, path, &minio)
                    .instrument(span)
                    .await
                    .ok();
            }
            StreamMessage::StreamStarted { live_id } => {
                stream_started_handler(live_id, &client_factory)
                    .instrument(span)
                    .await;
            }
            StreamMessage::StreamStopped { live_id, error } => {
                stream_stopped_handler(live_id, error, &client_factory, &factory)
                    .instrument(span)
                    .await;
            }
            StreamMessage::StreamRestarting { live_id, error } => {
                stream_restarting_handler(live_id, error, &client_factory)
                    .instrument(span)
                    .await;
            }
            StreamMessage::PullerStarted { live_id } => {
                puller_started_handler(live_id, &client_factory)
                    .instrument(span)
                    .await;
            }
            StreamMessage::PullerStopped { live_id } => {
                puller_stopped_handler(live_id, &client_factory)
                    .instrument(span)
                    .await;
            }
        }
    }
}

async fn segment_complete_handler(
    live_id: String,
    path: PathBuf,
    minio: &MinioClient,
) -> anyhow::Result<()> {
    event!(Level::DEBUG, "Segment complete: live_id={}", live_id);

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

async fn stream_started_handler(live_id: String, client_factory: &GrpcClientFactory) {
    event!(Level::INFO, "Stream started: live_id={}", live_id);

    if let Ok(Some(mut client)) = client_factory.build().await {
        let req = NotifyStreamStartedRequest { live_id };
        if let Err(e) = client.notify_stream_started(req).await {
            warn!(error = %e, "Failed to notify stream started");
        }
    } else {
        warn!(client = %client_factory, "Failed to connect to gRPC callback");
    }
}

async fn stream_stopped_handler(
    live_id: String,
    error_message: Option<String>,
    client_factory: &GrpcClientFactory,
    factory: &Arc<StreamPullerFactory>,
) {
    event!(
        Level::INFO,
        "Stream stopped: live_id={}, error={}",
        live_id,
        error_message.as_deref().unwrap_or("None")
    );

    factory.remove_signal(&live_id).await;

    if let Ok(Some(mut client)) = client_factory.build().await {
        let req = NotifyStreamStoppedRequest {
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

async fn stream_restarting_handler(
    live_id: String,
    error_message: String,
    client_factory: &GrpcClientFactory,
) {
    event!(
        Level::INFO,
        "Stream restarting: live_id={}, error={}",
        live_id,
        error_message
    );

    if let Ok(Some(mut client)) = client_factory.build().await {
        let req = NotifyStreamRestartingRequest {
            live_id,
            error_message,
        };
        if let Err(e) = client.notify_stream_restarting(req).await {
            warn!(error = %e, "Failed to notify stream restarting");
        }
    } else {
        warn!(client = %client_factory, "Failed to connect to gRPC callback");
    }
}

async fn puller_started_handler(live_id: String, client_factory: &GrpcClientFactory) {
    event!(Level::INFO, "Puller started: live_id={}", live_id);

    if let Ok(Some(mut client)) = client_factory.build().await {
        let req = NotifyPullerStartedRequest { live_id };
        if let Err(e) = client.notify_puller_started(req).await {
            warn!(error = %e, "Failed to notify puller started");
        }
    } else {
        warn!(client = %client_factory, "Failed to connect to gRPC callback");
    }
}

async fn puller_stopped_handler(live_id: String, client_factory: &GrpcClientFactory) {
    event!(Level::INFO, "Puller stopped: live_id={}", live_id);

    if let Ok(Some(mut client)) = client_factory.build().await {
        let req = NotifyPullerStoppedRequest { live_id };
        if let Err(e) = client.notify_puller_stopped(req).await {
            warn!(error = %e, "Failed to notify puller stopped");
        }
    } else {
        warn!(client = %client_factory, "Failed to connect to gRPC callback");
    }
}
