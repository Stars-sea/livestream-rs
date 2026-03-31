use anyhow::Result;

use crate::abstraction::MiddlewareTrait;
use crate::infra::MinioClient;
use crate::pipeline::UnifiedPacketContext;

pub struct PersistenceMiddleware {
    minio_client: MinioClient,
}

impl PersistenceMiddleware {
    pub fn new(minio_client: MinioClient) -> Self {
        Self { minio_client }
    }
}

#[async_trait::async_trait]
impl MiddlewareTrait for PersistenceMiddleware {
    type Context = UnifiedPacketContext;

    async fn send(&self, ctx: Self::Context) -> Result<Self::Context> {
        Ok(ctx)
    }
}
