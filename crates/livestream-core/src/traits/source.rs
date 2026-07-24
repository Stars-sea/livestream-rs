use anyhow::Result;
use async_trait::async_trait;

use crate::pad::PadSender;
use crate::traits::Node;
use crate::types::{CodecParams, MediaPacket, Protocol};

/// Produces media packets from a transport protocol (RTMP, RTSP).
///
/// On `start()` returning `Err`, the pipeline initiates a full shutdown.
#[async_trait]
pub trait Source: Node {
    type Output: MediaPacket;

    fn protocol(&self) -> Protocol;
    fn codec_params(&self) -> &[CodecParams];
    fn output(&self) -> &PadSender<Self::Output>;

    /// Begin producing packets. Loops until EOF or error.
    async fn start(&self) -> Result<()>;

    /// Stop producing and clean up transport resources.
    async fn stop(&self) -> Result<()>;
}
