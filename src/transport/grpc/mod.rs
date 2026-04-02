pub mod api {
    tonic::include_proto!("livestream");
}

mod server;

pub use server::GrpcServer;
