mod client;
mod storage;

pub mod api {
    tonic::include_proto!("livestream");
}

pub use client::grpc::GrpcClientFactory;
pub use storage::cache::MemoryCache;
pub use storage::minio::MinioClient;
