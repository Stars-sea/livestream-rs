mod registry;
pub mod rtmp;
mod session;
pub mod srt;

pub use registry::{ConnectionRegistry, global_registry};
pub use session::*;

pub fn init() {
    registry::init_registry();
}
