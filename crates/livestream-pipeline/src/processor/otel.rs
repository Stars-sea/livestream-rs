//! OTelProbe — telemetry passthrough processor.
//!
//! Emits metrics for every packet flowing through the pipeline.
//! Does not modify data.

use anyhow::Result;
use livestream_telemetry::metric_pipeline_packet;
use livestream_codec::EncodedPacket;
use livestream_core::{
    pad::{PadReceiver, PadSender},
    traits::{Node, Processor},
    types::CodecParams,
};

/// Observability probe.  Records per-packet metrics and passes data through
/// unchanged.  Always active (`should_process()` returns `true`).
pub struct OTelProbe {
    stream_id: String,
    input: PadReceiver<EncodedPacket>,
    outputs: Vec<PadSender<EncodedPacket>>,
}

impl OTelProbe {
    pub fn new(
        stream_id: &str,
        input: PadReceiver<EncodedPacket>,
        outputs: Vec<PadSender<EncodedPacket>>,
    ) -> Self {
        Self {
            stream_id: stream_id.into(),
            input,
            outputs,
        }
    }
}

impl Node for OTelProbe {
    fn name(&self) -> &str {
        "otel-probe"
    }
}

#[async_trait::async_trait]
impl Processor for OTelProbe {
    type Input = EncodedPacket;
    type Output = EncodedPacket;

    fn input_codec(&self) -> &[CodecParams] {
        &[]
    }

    fn output_codec(&self) -> &[CodecParams] {
        &[]
    }

    fn input(&self) -> &PadReceiver<Self::Input> {
        &self.input
    }

    fn outputs(&self) -> &[PadSender<Self::Output>] {
        &self.outputs
    }

    fn should_process(&self) -> bool {
        true // always observe
    }

    async fn process(&self, pkt: Self::Input) -> Result<Vec<Self::Output>> {
        tracing::debug!(
            stream = %self.stream_id,
            codec = ?pkt.codec,
            bytes = pkt.byte_size(),
            keyframe = pkt.is_keyframe,
            "pipeline packet"
        );

        metric_pipeline_packet!("encoded", pkt.byte_size());
        Ok(vec![pkt])
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use livestream_core::pad::PadSender;

    #[test]
    fn otel_probe_passthrough() {
        let (_tx, rx) = PadSender::<EncodedPacket>::new_channel(4);
        let probe = OTelProbe::new("test-stream", rx, vec![]);
        assert_eq!(probe.name(), "otel-probe");
        assert!(probe.should_process());
    }
}
