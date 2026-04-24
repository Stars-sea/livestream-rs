#[allow(clippy::module_inception)]
mod registry;
pub mod state;

pub use registry::INSTANCE;
