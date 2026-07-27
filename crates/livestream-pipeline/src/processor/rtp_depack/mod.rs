//! RTP depacketizer processor using FFmpeg's RTP demuxer.
//!
//! Supports **all** codecs FFmpeg's RTP demuxer supports:
//! H.264, H.265, AAC, MJPEG, VP8, VP9, Opus, MP3, and more.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

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
    demuxer: Mutex<RtpDemuxContext>,
    input: PadReceiver<RtpPacket>,
    outputs: Vec<PadSender<EncodedPacket>>,
    /// Tracks whether codec sequence headers (SPS+PPS / ASC) have been sent.
    /// Set to true after the first process() call emits them.
    sent_seq_header: AtomicBool,
}

impl RtpDemuxProcessor {
    pub fn new(
        _stream_id: &str,
        sdp: &str,
        _codec_params: Vec<CodecParams>,
        input: PadReceiver<RtpPacket>,
        outputs: Vec<PadSender<EncodedPacket>>,
    ) -> Result<Self> {
        let demuxer = RtpDemuxContext::new(sdp)?;
        Ok(Self {
            demuxer: Mutex::new(demuxer),
            input,
            outputs,
            sent_seq_header: AtomicBool::new(false),
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
        &[]
    }

    fn input(&self) -> &PadReceiver<Self::Input> {
        &self.input
    }

    fn outputs(&self) -> &[PadSender<Self::Output>] {
        &self.outputs
    }

    /// RTP demux must always run — downstream demand flows through
    /// the encoded packet chain where OTelProbe/SeqCacheProbe each
    /// return `should_process() == true`. If we gated on output pad
    /// demand, we'd deadlock because no subscriber creates a demand
    /// handle on this pad.
    fn should_process(&self) -> bool {
        true
    }

    async fn process(&self, input: Self::Input) -> Result<Vec<Self::Output>> {
        let demuxer = self.demuxer.lock().unwrap();

        // Emit codec sequence headers (SPS+PPS / ASC) on first call.
        // The RTP demuxer's stream-level codecpar holds the extradata;
        // we extract it once and send it as a synthetic header packet so
        // that downstream decoders (FLV, HLS) can initialize properly.
        let mut packets = if !self.sent_seq_header.swap(true, Ordering::AcqRel) {
            let mut headers = Vec::new();
            for si in 0..demuxer.stream_count() {
                if let (Some(codec), Some(extradata)) =
                    (demuxer.codec_for_stream(si), demuxer.extradata(si))
                {
                    let header = EncodedPacket {
                        codec,
                        stream_index: si,
                        data: bytes::Bytes::new(),
                        pts_ms: None,
                        dts_ms: None,
                        is_keyframe: true,
                        is_sequence_header: true,
                        is_script_data: false,
                        extradata: Some(extradata),
                    };
                    tracing::info!(
                        codec = ?codec,
                        stream = si,
                        extradata_len = header.extradata.as_ref().map(|b| b.len()).unwrap_or(0),
                        "RtpDemuxProcessor emitting sequence header"
                    );
                    headers.push(header);
                }
            }
            headers
        } else {
            Vec::new()
        };

        demuxer.feed(&input.data)?;

        while let Some((mut pkt, codec, tb)) = demuxer.read_frame()? {
            let encoded = EncodedPacket::from_av_packet(&mut pkt, tb, codec)?;
            drop(pkt);
            packets.push(encoded);
        }

        Ok(packets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
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

    #[tokio::test]
    async fn feed_and_receive() {
        let sdp = "v=0\r\n\
            o=- 0 0 IN IP4 127.0.0.1\r\n\
            s=Test\r\n\
            c=IN IP4 127.0.0.1\r\n\
            t=0 0\r\n\
            m=video 5000 RTP/AVP 96\r\n\
            a=rtpmap:96 H264/90000\r\n";
        let (rtp_tx, rtp_rx) = PadSender::<RtpPacket>::new_channel(256);
        let (enc_tx, _enc_rx) = PadSender::<EncodedPacket>::new_channel(256);
        let proc =
            Arc::new(RtpDemuxProcessor::new("test", sdp, vec![], rtp_rx, vec![enc_tx]).unwrap());
        let pkt = RtpPacket::new(96, 0, 0, false, 1, 0, &[0u8; 12]);
        rtp_tx.send(pkt.clone()).unwrap();
        drop(rtp_tx);
        let received = proc.input().recv().await;
        assert!(received.is_some(), "PadReceiver should receive RTP packet");
        let result = proc.process(received.unwrap()).await;
        assert!(
            result.is_ok(),
            "process() should succeed: {:?}",
            result.err()
        );
    }
}
