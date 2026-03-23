mod client;
mod shutdown;
mod storage;

pub mod api {
    tonic::include_proto!("livestream");
}

pub use client::grpc::GrpcClientFactory;
pub use shutdown::ShutdownManager;
pub use storage::minio::MinioClient;
