use std::future::Future;
use std::path::PathBuf;

use tokio::fs;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{Instrument, Level, debug, warn};
use tracing::{Span, event};

use super::types::*;
use crate::infra::GrpcClientFactory;
use crate::infra::MinioClient;
use crate::infra::api::*;

pub(crate) async fn stream_message_handler(
    rx: mpsc::UnboundedReceiver<(StreamMessage, Span)>,
    client_factory: GrpcClientFactory,
    minio: MinioClient,
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
                stream_stopped_handler(live_id, error, &client_factory)
                    .instrument(span)
                    .await;
            }
            StreamMessage::StreamRestarting { live_id, error } => {
                stream_restarting_handler(live_id, error, &client_factory)
                    .instrument(span)
                    .await;
            }
            StreamMessage::IngestWorkerStarted { live_id } => {
                ingest_worker_started_handler(live_id, &client_factory)
                    .instrument(span)
                    .await;
            }
            StreamMessage::IngestWorkerStopped { live_id } => {
                ingest_worker_stopped_handler(live_id, &client_factory)
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
    let uploader = |key: String, local_path: PathBuf| async move {
        minio.upload_file(&key, local_path.as_path()).await
    };

    segment_complete_handler_with_uploader(live_id, path, &uploader).await
}

async fn segment_complete_handler_with_uploader<F, Fut>(
    live_id: String,
    path: PathBuf,
    uploader: &F,
) -> anyhow::Result<()>
where
    F: Fn(String, PathBuf) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    event!(Level::DEBUG, "Segment complete: live_id={}", live_id);

    let filename = path
        .file_name()
        .ok_or(anyhow::anyhow!("Failed to get file name"))?;
    let storage_key = format!("{}/{}", live_id, filename.to_string_lossy());

    let canonical = fs::canonicalize(&path).await?;
    let upload_resp = uploader(storage_key, canonical).await;

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
            client_factory.invalidate().await;
        }
    } else {
        warn!(client = %client_factory, "Failed to connect to gRPC callback");
    }
}

async fn stream_stopped_handler(
    live_id: String,
    error_message: Option<String>,
    client_factory: &GrpcClientFactory,
) {
    event!(
        Level::INFO,
        "Stream stopped: live_id={}, error={}",
        live_id,
        error_message.as_deref().unwrap_or("None")
    );

    if let Ok(Some(mut client)) = client_factory.build().await {
        let req = NotifyStreamStoppedRequest {
            live_id,
            error_message,
        };
        if let Err(e) = client.notify_stream_stopped(req).await {
            warn!(error = %e, "Failed to notify stream stopped");
            client_factory.invalidate().await;
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
            client_factory.invalidate().await;
        }
    } else {
        warn!(client = %client_factory, "Failed to connect to gRPC callback");
    }
}

async fn ingest_worker_started_handler(live_id: String, client_factory: &GrpcClientFactory) {
    event!(Level::INFO, "Ingest worker started: live_id={}", live_id);

    if let Ok(Some(mut client)) = client_factory.build().await {
        let req = NotifyIngestWorkerStartedRequest { live_id };
        if let Err(e) = client.notify_ingest_worker_started(req).await {
            warn!(error = %e, "Failed to notify ingest worker started");
            client_factory.invalidate().await;
        }
    } else {
        warn!(client = %client_factory, "Failed to connect to gRPC callback");
    }
}

async fn ingest_worker_stopped_handler(live_id: String, client_factory: &GrpcClientFactory) {
    event!(Level::INFO, "Ingest worker stopped: live_id={}", live_id);

    if let Ok(Some(mut client)) = client_factory.build().await {
        let req = NotifyIngestWorkerStoppedRequest { live_id };
        if let Err(e) = client.notify_ingest_worker_stopped(req).await {
            warn!(error = %e, "Failed to notify ingest worker stopped");
            client_factory.invalidate().await;
        }
    } else {
        warn!(client = %client_factory, "Failed to connect to gRPC callback");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn segment_upload_failure_keeps_file_for_retry() {
        let dir = tempfile::tempdir().unwrap();
        let segment_path = dir.path().join("seg.ts");
        tokio::fs::write(&segment_path, b"segment").await.unwrap();

        let uploader = |_key: String, _path: PathBuf| async {
            Err(anyhow::anyhow!("transient storage outage"))
        };

        let result = segment_complete_handler_with_uploader(
            "live_test".to_string(),
            segment_path.clone(),
            &uploader,
        )
        .await;

        assert!(result.is_err());
        assert!(segment_path.exists());
    }

    #[tokio::test]
    async fn segment_upload_transient_outage_then_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let segment_path = dir.path().join("seg.ts");
        tokio::fs::write(&segment_path, b"segment").await.unwrap();

        let attempts = Arc::new(AtomicUsize::new(0));
        let uploader_attempts = attempts.clone();
        let uploader = move |_key: String, _path: PathBuf| {
            let current = uploader_attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                if current == 0 {
                    Err(anyhow::anyhow!("temporary outage"))
                } else {
                    Ok(())
                }
            }
        };

        let first = segment_complete_handler_with_uploader(
            "live_test".to_string(),
            segment_path.clone(),
            &uploader,
        )
        .await;
        assert!(first.is_err());
        assert!(segment_path.exists());

        let second = segment_complete_handler_with_uploader(
            "live_test".to_string(),
            segment_path.clone(),
            &uploader,
        )
        .await;
        assert!(second.is_ok());
        assert!(!segment_path.exists());
    }

    #[tokio::test]
    async fn callback_factory_empty_url_is_non_fatal() {
        let factory = GrpcClientFactory::new(String::new());
        stream_started_handler("live_test".to_string(), &factory).await;
        stream_stopped_handler("live_test".to_string(), None, &factory).await;
        stream_restarting_handler("live_test".to_string(), "err".to_string(), &factory).await;
    }
}
