use crate::infra;
use crate::pipeline::Pipe;
use crate::pipeline::UnifiedPacketContext;
use crate::pipeline::middleware::PersistenceMiddleware;
use crate::pipeline::middleware::{BroadcastMiddleware, SegmentMiddleware};
use crate::pipeline::pipe::PipeFactory;

pub struct UnifiedPipeFactory {
    minio_client: infra::MinioClient,
}

impl UnifiedPipeFactory {
    pub fn new(minio_client: infra::MinioClient) -> Self {
        Self { minio_client }
    }
}

impl PipeFactory for UnifiedPipeFactory {
    type Context = UnifiedPacketContext;

    fn create(&self) -> Pipe<Self::Context> {
        let minio_client = self.minio_client.clone();
        let persistence = PersistenceMiddleware::new(minio_client);
        let broadcast = BroadcastMiddleware::<UnifiedPacketContext>::new();
        let segment = SegmentMiddleware::new();

        Pipe::new().with(broadcast).with(segment).with(persistence)
    }
}
