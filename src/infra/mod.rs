mod client;
mod storage;
mod shutdown;

pub mod api {
    tonic::include_proto!("livestream");
}

pub use client::grpc::GrpcClientFactory;
pub use storage::cache::MemoryCache;
pub use storage::minio::MinioClient;
pub use shutdown::ShutdownManager;
