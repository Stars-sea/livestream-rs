mod cache;
mod grpc_server;
mod minio;
mod rtmp_server;

pub use cache::MemoryCache;
pub use grpc_server::GrpcServerFactory;
pub use minio::MinioClient;
pub use minio::MinioClientFactory;
pub use rtmp_server::RtmpServerFactory;

use anyhow::Result;
use std::env::var;

pub fn env_var(key: &str) -> Result<String> {
    var(key).map_err(|_| anyhow::anyhow!("{} environment variable not set", key))
}
