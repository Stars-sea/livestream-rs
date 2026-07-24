use anyhow::Result;
use async_trait::async_trait;

use crate::pad::{PadReceiver, PadSender};
use crate::traits::Node;
use crate::types::{CodecParams, MediaPacket};

/// Transforms or inspects media packets.
///
/// `process()` errors are non-fatal: the error is logged, a metric is
/// incremented, and the packet is dropped. The pipeline continues.
#[async_trait]
pub trait Processor: Node {
    type Input: MediaPacket;
    type Output: MediaPacket;

    fn input_codec(&self) -> &[CodecParams];
    fn output_codec(&self) -> &[CodecParams];
    fn input(&self) -> &PadReceiver<Self::Input>;
    fn outputs(&self) -> &[PadSender<Self::Output>];

    /// Whether this processor should do work.
    /// Default: checks if any output pad has downstream demand.
    fn should_process(&self) -> bool {
        self.outputs().iter().any(|p| p.demand().is_wanted())
    }

    /// Process one input packet. Returns zero or more output packets.
    /// On `Err` → packet dropped, pipeline continues.
    async fn process(&self, input: Self::Input) -> Result<Vec<Self::Output>>;

    /// Flush internal state on pipeline shutdown (e.g., encoder flush).
    async fn close(&self) -> Result<()> {
        Ok(())
    }
}
