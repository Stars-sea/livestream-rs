//! Transcode — EncodedPacket → EncodedPacket codec transformation (DEFERRED).
//!
//! Placeholder for Phase 4.5 / Phase 6. Uses Decoder, Encoder, Scaler from
//! `livestream-media`.

use anyhow::Result;
use livestream_codec::EncodedPacket;
use livestream_core::{
    pad::{PadReceiver, PadSender},
    traits::{Node, Processor},
    types::CodecParams,
};

pub struct Transcode {
    _input: PadReceiver<EncodedPacket>,
    _outputs: Vec<PadSender<EncodedPacket>>,
}

impl Transcode {
    #[allow(unused)]
    pub fn new(input: PadReceiver<EncodedPacket>, outputs: Vec<PadSender<EncodedPacket>>) -> Self {
        Self {
            _input: input,
            _outputs: outputs,
        }
    }
}

impl Node for Transcode {
    fn name(&self) -> &str {
        "transcode"
    }
}

#[async_trait::async_trait]
impl Processor for Transcode {
    type Input = EncodedPacket;
    type Output = EncodedPacket;

    fn input_codec(&self) -> &[CodecParams] {
        &[]
    }

    fn output_codec(&self) -> &[CodecParams] {
        &[]
    }

    fn input(&self) -> &PadReceiver<Self::Input> {
        &self._input
    }

    fn outputs(&self) -> &[PadSender<Self::Output>] {
        &self._outputs
    }

    async fn process(&self, pkt: Self::Input) -> Result<Vec<Self::Output>> {
        // DEFERRED: Phase 4.5 / Phase 6
        let _ = pkt;
        Ok(vec![])
    }
}
