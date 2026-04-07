pub mod api {
    tonic::include_proto!("livestream");
}

#[cfg(feature = "opentelemetry")]
mod extractor;
mod server;

pub use server::GrpcServer;
