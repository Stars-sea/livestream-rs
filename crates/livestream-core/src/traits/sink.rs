use anyhow::Result;
use async_trait::async_trait;

use crate::pad::{DemandHandle, PadReceiver};
use crate::traits::Node;
use crate::types::{CodecParams, MediaPacket, Protocol};

/// Consumes media packets — the terminal node of a pipeline branch.
///
/// `consume()` errors are non-fatal: logged and metered. The sink continues.
#[async_trait]
pub trait Sink: Node {
    type Input: MediaPacket;

    fn protocol(&self) -> Protocol;
    fn accepted_codec(&self) -> &[CodecParams];
    fn input(&self) -> &PadReceiver<Self::Input>;
    fn demand_handle(&self) -> &DemandHandle;

    /// Consume one item.
    async fn consume(&self, input: Self::Input) -> Result<()>;
}
