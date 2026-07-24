mod pipeline;
mod processor;
mod sink;
mod source;

pub use pipeline::{Pipeline, PipelineHandle, PipelineState};
pub use processor::Processor;
pub use sink::Sink;
pub use source::Source;

/// Base trait for all pipeline nodes.
pub trait Node: Send + Sync {
    /// Human-readable name for logging and metrics.
    fn name(&self) -> &str;
}
