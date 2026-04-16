pub mod api {
    tonic::include_proto!("livestream");
}

#[cfg(feature = "opentelemetry")]
mod context_propagation;
mod server;

pub use server::GrpcServer;
