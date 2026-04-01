use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::AppConfig;
use crate::infra;
use crate::pipeline::Pipe;
use crate::pipeline::UnifiedPacketContext;
use crate::pipeline::handler::SegmentPersistenceHandler;
use crate::pipeline::middleware::{BroadcastMiddleware, SegmentMiddleware};
use crate::pipeline::pipe::PipeFactory;

pub struct UnifiedPipeFactory {
    minio_client: infra::MinioClient,
    segment_duration: Duration,
}

impl UnifiedPipeFactory {
    pub async fn new(config: &AppConfig) -> Result<Self> {
        let minio = config
            .minio
            .clone()
            .context("MinIO configuration is missing")?;
        let minio_client = infra::MinioClient::create(minio).await?;
        let segment_duration = Duration::from_secs(config.srt.duration.max(1) as u64);

        Ok(Self {
            minio_client,
            segment_duration,
        })
    }
}

impl PipeFactory for UnifiedPipeFactory {
    type Context = UnifiedPacketContext;

    fn create(&self) -> Pipe<Self::Context> {
        let minio_client = self.minio_client.clone();
        SegmentPersistenceHandler::spawn(minio_client);
        let broadcast = BroadcastMiddleware::<UnifiedPacketContext>::new();
        let segment = SegmentMiddleware::new(self.segment_duration);

        Pipe::new().with(broadcast).with(segment)
    }
}
