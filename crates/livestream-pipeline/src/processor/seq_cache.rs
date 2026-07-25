//! SeqCacheProbe — caches sequence headers for late-joining subscribers.

use anyhow::Result;
use livestream_codec::EncodedPacket;
use livestream_core::{
    pad::{PadReceiver, PadSender},
    traits::{Node, Processor},
    types::CodecParams,
};
use parking_lot::Mutex;

pub struct SeqCacheProbe {
    video_seq_header: Mutex<Option<EncodedPacket>>,
    audio_seq_header: Mutex<Option<EncodedPacket>>,
    last_keyframe: Mutex<Option<EncodedPacket>>,
    script_data: Mutex<Option<EncodedPacket>>,
    input: PadReceiver<EncodedPacket>,
    outputs: Vec<PadSender<EncodedPacket>>,
}

impl SeqCacheProbe {
    pub fn new(input: PadReceiver<EncodedPacket>, outputs: Vec<PadSender<EncodedPacket>>) -> Self {
        Self {
            video_seq_header: Mutex::new(None),
            audio_seq_header: Mutex::new(None),
            last_keyframe: Mutex::new(None),
            script_data: Mutex::new(None),
            input,
            outputs,
        }
    }

    pub fn snapshot(&self) -> Vec<EncodedPacket> {
        let mut result = Vec::with_capacity(4);
        if let Some(d) = self.script_data.lock().clone() {
            result.push(d);
        }
        if let Some(d) = self.video_seq_header.lock().clone() {
            result.push(d);
        }
        if let Some(d) = self.audio_seq_header.lock().clone() {
            result.push(d);
        }
        if let Some(d) = self.last_keyframe.lock().clone() {
            result.push(d);
        }
        result
    }
}

impl Node for SeqCacheProbe {
    fn name(&self) -> &str {
        "seq-cache-probe"
    }
}

#[async_trait::async_trait]
impl Processor for SeqCacheProbe {
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
        true
    }

    async fn process(&self, pkt: Self::Input) -> Result<Vec<Self::Output>> {
        if pkt.is_video() && pkt.is_sequence_header {
            *self.video_seq_header.lock() = Some(pkt.clone());
        } else if pkt.is_audio() && pkt.is_sequence_header {
            *self.audio_seq_header.lock() = Some(pkt.clone());
        } else if pkt.is_video() && pkt.is_keyframe {
            *self.last_keyframe.lock() = Some(pkt.clone());
        } else if pkt.is_script_data {
            *self.script_data.lock() = Some(pkt.clone());
        }
        Ok(vec![pkt])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use livestream_core::pad::PadSender;

    fn make_test_probe() -> SeqCacheProbe {
        let (_tx, rx) = PadSender::<EncodedPacket>::new_channel(4);
        SeqCacheProbe::new(rx, vec![])
    }

    #[test]
    fn caches_video_seq_header() {
        let probe = make_test_probe();
        assert!(probe.snapshot().is_empty());
        *probe.video_seq_header.lock() =
            Some(EncodedPacket::new_avc_sequence_header(&[0x67], &[0x68], 0));
        assert_eq!(probe.snapshot().len(), 1);
    }

    #[test]
    fn snapshot_order() {
        let probe = make_test_probe();
        let mut script = EncodedPacket::new_video_keyframe(Bytes::from_static(&[0x00]), 0, 0, 0);
        script.is_script_data = true;
        *probe.script_data.lock() = Some(script);
        *probe.video_seq_header.lock() =
            Some(EncodedPacket::new_avc_sequence_header(&[0x67], &[0x68], 0));

        let snap = probe.snapshot();
        assert_eq!(snap.len(), 2);
        assert!(snap[0].is_script_data);
    }
}
