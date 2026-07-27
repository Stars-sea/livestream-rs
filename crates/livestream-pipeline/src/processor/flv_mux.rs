//! FlvMux — pure-Rust FLV muxer: EncodedPacket → FlvTag.
//!
//! No FFmpeg dependency. Lazy: `should_process()` checks output demand,
//! skipping work when no FLV subscribers are connected.

use anyhow::Result;
use bytes::{BufMut, Bytes, BytesMut};
use livestream_codec::EncodedPacket;

trait PutU24 {
    fn put_u24(&mut self, value: u32);
}

impl PutU24 for BytesMut {
    fn put_u24(&mut self, value: u32) {
        self.put_u8(((value >> 16) & 0xff) as u8);
        self.put_u8(((value >> 8) & 0xff) as u8);
        self.put_u8((value & 0xff) as u8);
    }
}
use livestream_core::{
    pad::{PadReceiver, PadSender},
    traits::{Node, Processor},
    types::{Codec, CodecParams},
};
use livestream_media::flv::FlvTag;
use livestream_telemetry::metric_pipeline_error;

pub struct FlvMux {
    stream_id: String,
    input: PadReceiver<EncodedPacket>,
    outputs: Vec<PadSender<FlvTag>>,
}

impl FlvMux {
    pub fn new(
        stream_id: &str,
        input: PadReceiver<EncodedPacket>,
        outputs: Vec<PadSender<FlvTag>>,
    ) -> Self {
        Self {
            stream_id: stream_id.into(),
            input,
            outputs,
        }
    }

    fn mux_video(&self, pkt: &EncodedPacket) -> Result<Vec<FlvTag>> {
        // Clamp negative PTS to 0 and cap at u32::MAX
        let timestamp = (pkt.pts_ms.unwrap_or(0).max(0) as u64).min(u32::MAX as u64) as u32;
        let is_keyframe = pkt.is_keyframe;
        let is_seq_header = pkt.is_sequence_header;

        let payload = if is_seq_header {
            self.build_avc_sequence_header(&pkt.data, pkt.extradata.as_deref())
        } else {
            self.annex_b_to_avcc(&pkt.data)
        };

        let frame_type: u8 = if is_keyframe { 1 } else { 2 };
        // FLV CodecID: 7=AVC(H.264), 12=HEVC(H.265)
        let codec_id: u8 = match pkt.codec {
            Codec::H264 => 7,
            Codec::H265 => 12,
            _ => 7, // unreachable — process() only routes H264/H265 here
        };
        let mut tag_payload = BytesMut::new();
        tag_payload.put_u8((frame_type << 4) | codec_id);

        if is_seq_header {
            tag_payload.put_u8(0); // AVCPacketType = 0
            tag_payload.put_u24(0); // CompositionTime
        } else {
            tag_payload.put_u8(1); // AVCPacketType = 1
            tag_payload.put_u24(0); // CompositionTime
        }
        tag_payload.extend_from_slice(&payload);

        let tag_bytes = tag_payload.freeze();
        let tag = FlvTag::video(timestamp, tag_bytes);
        Ok(vec![tag])
    }

    fn mux_audio(&self, pkt: &EncodedPacket) -> Result<Vec<FlvTag>> {
        let timestamp = (pkt.pts_ms.unwrap_or(0).max(0) as u64).min(u32::MAX as u64) as u32;
        let is_seq_header = pkt.is_sequence_header;

        // SoundFormat=10(AAC) | SoundRate=3(44kHz) | SoundSize=1(16bit) | SoundType=1(stereo)
        let sound_format: u8 = 10;
        let sound_byte = (sound_format << 4) | 0x0F;

        let aac_packet_type: u8 = if is_seq_header { 0 } else { 1 };
        let mut tag_payload = BytesMut::new();
        tag_payload.put_u8(sound_byte);
        tag_payload.put_u8(aac_packet_type);
        tag_payload.extend_from_slice(&pkt.data);

        let tag = FlvTag::audio(timestamp, tag_payload.freeze());
        Ok(vec![tag])
    }

    fn build_avc_sequence_header(&self, _data: &[u8], extradata: Option<&[u8]>) -> Bytes {
        // AVCDecoderConfigurationRecord = extradata (SPS+PPS from codec)
        if let Some(ext) = extradata {
            Bytes::copy_from_slice(ext)
        } else {
            Bytes::new()
        }
    }

    fn annex_b_to_avcc(&self, data: &[u8]) -> Bytes {
        // Convert Annex B (00 00 01 start codes) to AVCC (4-byte length prefix).
        // For now, preserve as-is — full conversion deferred to Phase 5.
        Bytes::copy_from_slice(data)
    }
}

impl Node for FlvMux {
    fn name(&self) -> &str {
        "flv-mux"
    }
}

#[async_trait::async_trait]
impl Processor for FlvMux {
    type Input = EncodedPacket;
    type Output = FlvTag;

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

    async fn process(&self, pkt: Self::Input) -> Result<Vec<Self::Output>> {
        match pkt.codec {
            Codec::H264 | Codec::H265 => self.mux_video(&pkt),
            Codec::Aac => self.mux_audio(&pkt),
            other => {
                metric_pipeline_error!("flv_mux.unsupported_codec");
                tracing::warn!(
                    stream = %self.stream_id,
                    codec = ?other,
                    "FlvMux received unsupported codec; dropping packet"
                );
                Ok(vec![])
            }
        }
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use livestream_core::pad::PadSender;

    fn make_mux() -> FlvMux {
        let (_tx, rx) = PadSender::<EncodedPacket>::new_channel(4);
        FlvMux::new("test", rx, vec![])
    }

    #[test]
    fn mux_video_keyframe() {
        let mux = make_mux();
        let pkt =
            EncodedPacket::new_video_keyframe(&[0x00, 0x00, 0x01, 0x65, 0x88][..], 1000, 900, 0);
        let tags = mux.mux_video(&pkt).unwrap();
        assert_eq!(tags.len(), 1);
        match &tags[0] {
            FlvTag::Video {
                timestamp,
                payload,
                is_keyframe,
            } => {
                assert_eq!(*timestamp, 1000);
                assert!(*is_keyframe);
                // First byte should be FrameType=1(keyframe) | CodecID=7(AVC) = 0x17
                assert_eq!(payload[0], 0x17);
                // Second byte should be AVCPacketType=1 (NAL unit)
                assert_eq!(payload[1], 0x01);
            }
            _ => panic!("expected video tag"),
        }
    }

    #[test]
    fn mux_audio() {
        let mux = make_mux();
        let pkt = EncodedPacket::new_audio(&[0x21, 0x10, 0x04, 0x60][..], 500, 1);
        let tags = mux.mux_audio(&pkt).unwrap();
        assert_eq!(tags.len(), 1);
        match &tags[0] {
            FlvTag::Audio { timestamp, payload } => {
                assert_eq!(*timestamp, 500);
                // First byte: SoundFormat=10(AAC) | 0x0F
                assert_eq!(payload[0], 0xAF);
                // Second byte: AACPacketType=1
                assert_eq!(payload[1], 0x01);
            }
            _ => panic!("expected audio tag"),
        }
    }

    #[tokio::test]
    async fn unsupported_codec_drops_packet() {
        let mux = make_mux();
        let pkt = EncodedPacket {
            codec: Codec::Av1,
            stream_index: 0,
            data: Bytes::from_static(&[0x00]),
            pts_ms: Some(0),
            dts_ms: None,
            is_keyframe: true,
            is_sequence_header: false,
            is_script_data: false,
            extradata: None,
        };
        let tags = mux.process(pkt).await.unwrap();
        assert!(tags.is_empty());
    }
}
