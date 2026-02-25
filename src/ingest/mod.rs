mod events;
mod handlers;
mod manager;
mod port_allocator;
mod pull_stream;
mod service;
mod settings;
mod stream_info;

mod grpc {
    tonic::include_proto!("livestream");
}

pub use manager::StreamManager;
pub use service::{LivestreamServer, LivestreamService};
