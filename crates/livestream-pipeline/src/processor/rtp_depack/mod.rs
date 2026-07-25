//! RTP depacketizer processor using FFmpeg's RTP demuxer.
//!
//! Supports **all** codecs FFmpeg's RTP demuxer supports:
//! H.264, H.265, AAC, MJPEG, VP8, VP9, Opus, MP3, and more.

use std::sync::Mutex;

use anyhow::Result;
use livestream_codec::{EncodedPacket, RtpPacket};
use livestream_core::{
    pad::{PadReceiver, PadSender},
    traits::{Node, Processor},
    types::CodecParams,
};
use livestream_media::convert::FromAvPacket;
use livestream_media::rtp::RtpDemuxContext;

pub struct RtpDemuxProcessor {
    stream_id: String,
    codec_params: Vec<CodecParams>,
    demuxer: Mutex<RtpDemuxContext>,
    input: PadReceiver<RtpPacket>,
    outputs: Vec<PadSender<EncodedPacket>>,
}

impl RtpDemuxProcessor {
    pub fn new(
        stream_id: &str,
        sdp: &str,
        codec_params: Vec<CodecParams>,
        input: PadReceiver<RtpPacket>,
        outputs: Vec<PadSender<EncodedPacket>>,
    ) -> Result<Self> {
        let demuxer = RtpDemuxContext::new(sdp)?;
        Ok(Self {
            stream_id: stream_id.into(),
            codec_params,
            demuxer: Mutex::new(demuxer),
            input,
            outputs,
        })
    }
}

impl Node for RtpDemuxProcessor {
    fn name(&self) -> &str {
        "rtp-demux"
    }
}

#[async_trait::async_trait]
impl Processor for RtpDemuxProcessor {
    type Input = RtpPacket;
    type Output = EncodedPacket;

    fn input_codec(&self) -> &[CodecParams] {
        &[]
    }

    fn output_codec(&self) -> &[CodecParams] {
        &self.codec_params
    }

    fn input(&self) -> &PadReceiver<Self::Input> {
        &self.input
    }

    fn outputs(&self) -> &[PadSender<Self::Output>] {
        &self.outputs
    }

    async fn process(&self, input: Self::Input) -> Result<Vec<Self::Output>> {
        let demuxer = self.demuxer.lock().unwrap();

        demuxer.feed(&input.data)?;

        let mut packets = Vec::new();
        while let Some((pkt, codec, tb)) = demuxer.read_frame()? {
            let encoded = EncodedPacket::from_av_packet(&pkt, tb, codec)?;
            drop(pkt);
            packets.push(encoded);
        }

        if packets.is_empty() {
            tracing::trace!(
                stream = %self.stream_id,
                channel = input.channel,
                "RTP packet fed, no complete frame yet"
            );
        }

        Ok(packets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_with_valid_sdp() {
        let sdp = "v=0\r\n\
            o=- 0 0 IN IP4 127.0.0.1\r\n\
            s=Test\r\n\
            c=IN IP4 127.0.0.1\r\n\
            t=0 0\r\n\
            m=video 5000 RTP/AVP 96\r\n\
            a=rtpmap:96 H264/90000\r\n";

        let (_tx, rx) = PadSender::<RtpPacket>::new_channel(4);
        let proc = RtpDemuxProcessor::new("test", sdp, vec![], rx, vec![]);
        assert!(proc.is_ok(), "Should create: {:?}", proc.err());
        assert_eq!(proc.unwrap().name(), "rtp-demux");
    }
}
