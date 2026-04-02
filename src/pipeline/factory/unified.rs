use std::time::Duration;

use anyhow::{Context, Result};
use crossfire::MTx;
use crossfire::mpsc::List;

use crate::config::AppConfig;
use crate::infra;
use crate::pipeline::Pipe;
use crate::pipeline::UnifiedPacketContext;
use crate::pipeline::handler::SegmentPersistenceHandler;
use crate::pipeline::middleware::{FlvMuxForwardMiddleware, SegmentMiddleware};
use crate::pipeline::pipe::PipeFactory;
use crate::transport::contract::message::StreamFlvTag;

pub struct UnifiedPipeFactory {
    minio_client: infra::MinioClient,
    segment_duration: Duration,
    rtmp_tag_tx: MTx<List<StreamFlvTag>>,
}

impl UnifiedPipeFactory {
    pub async fn new(config: &AppConfig, rtmp_tag_tx: MTx<List<StreamFlvTag>>) -> Result<Self> {
        let minio = config
            .minio
            .clone()
            .context("MinIO configuration is missing")?;
        let minio_client = infra::MinioClient::create(minio).await?;
        let segment_duration = Duration::from_secs(config.srt.duration.max(1) as u64);

        Ok(Self {
            minio_client,
            segment_duration,
            rtmp_tag_tx,
        })
    }
}

impl PipeFactory for UnifiedPipeFactory {
    type Context = UnifiedPacketContext;

    fn create(&self) -> Pipe<Self::Context> {
        let minio_client = self.minio_client.clone();
        SegmentPersistenceHandler::spawn(minio_client);

        Pipe::new()
            // .with(BroadcastMiddleware::new())
            .with(FlvMuxForwardMiddleware::new(self.rtmp_tag_tx.clone()))
            .with(SegmentMiddleware::new(self.segment_duration))
    }
}
