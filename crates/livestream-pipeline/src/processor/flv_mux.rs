//! FlvMux — pure-Rust FLV muxer: EncodedPacket → FlvTag.
//!
//! No FFmpeg dependency. Lazy: `should_process()` checks output demand,
//! skipping work when no FLV subscribers are connected.

use anyhow::Result;
use bytes::{BufMut, Bytes, BytesMut};
use livestream_codec::EncodedPacket;
use std::sync::atomic::{AtomicBool, Ordering};

use livestream_core::{
    pad::{PadReceiver, PadSender},
    traits::{Node, Processor},
    types::{Codec, CodecParams},
};
use livestream_media::flv::FlvTag;
use livestream_media::flv::put_u24;
use livestream_telemetry::metric_pipeline_error;

pub struct FlvMux {
    stream_id: String,
    input: PadReceiver<EncodedPacket>,
    outputs: Vec<PadSender<FlvTag>>,
    /// Whether this stream's H.264 sequence header carries an
    /// AVCDecoderConfigurationRecord (byte 0 == 0x01). Such streams must
    /// carry length-prefixed (AVCC) NALs in media tags; Annex B sources
    /// (RTMP) keep their framing untouched.
    avcc_framing: AtomicBool,
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
            avcc_framing: AtomicBool::new(false),
        }
    }

    fn mux_video(&self, pkt: &EncodedPacket) -> Result<Vec<FlvTag>> {
        // FLV video tag timestamp must be the decode timestamp (DTS), with
        // CompositionTime carrying PTS - DTS so players can reconstruct PTS.
        // Audio (which has no B-frames) keeps using PTS in mux_audio.
        let pts_ms = pkt.pts_ms.unwrap_or(0).max(0) as u64;
        // Fall back to PTS when DTS is unavailable.
        let dts_ms = (pkt.dts_ms.unwrap_or(pts_ms as i64)).max(0) as u64;
        // Clamp negative timestamps to 0 and cap at u32::MAX.
        let timestamp = dts_ms.min(u32::MAX as u64) as u32;
        // CompositionTime is 24-bit: PTS - DTS, clamped to [0, 0xFFFFFF].
        let composition_time = pts_ms.saturating_sub(dts_ms).min(0xFFFFFF) as u32;
        let is_keyframe = pkt.is_keyframe;
        let is_seq_header = pkt.is_sequence_header;

        let payload = if is_seq_header {
            let ext = pkt.extradata.as_deref();
            // avcC records (byte 0 == 1) signal length-prefixed media NALs.
            if pkt.codec == Codec::H264 && ext.is_some_and(|e| !e.is_empty() && e[0] == 0x01) {
                self.avcc_framing.store(true, Ordering::Relaxed);
            }
            self.build_avc_sequence_header(pkt.data.as_bytes(), ext)
        } else {
            self.annex_b_to_avcc(pkt.data.as_bytes())
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
            put_u24(&mut tag_payload, 0); // CompositionTime (0 for headers)
        } else {
            tag_payload.put_u8(1); // AVCPacketType = 1
            put_u24(&mut tag_payload, composition_time); // CompositionTime = PTS - DTS
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
        tag_payload.extend_from_slice(pkt.data.as_bytes());

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

    /// Convert Annex B to AVCC (length-prefixed) NAL framing.
    ///
    /// Only applies to streams whose sequence header is an avcC record —
    /// FLV media tags for those MUST be length-prefixed (ffmpeg's demuxer
    /// parses them per the avcC `lengthSizeMinusOne`). Annex B sources
    /// (RTMP, whose sequence header is raw SPS+PPS) keep their framing
    /// unchanged, which ffmpeg's demuxer detects and handles.
    fn annex_b_to_avcc(&self, data: &[u8]) -> Bytes {
        if !self.avcc_framing.load(Ordering::Relaxed) {
            return Bytes::copy_from_slice(data);
        }
        let mut out = BytesMut::with_capacity(data.len() + data.len() / 4);
        for nal in split_annexb(data) {
            out.put_u32(nal.len() as u32);
            out.extend_from_slice(nal);
        }
        out.freeze()
    }
}

/// Split an Annex B bitstream into NAL units (skipping `00 00 01` and
/// `00 00 00 01` start codes). Small private helper duplicated here rather
/// than exported from `transcode` (same convention as `av_codec_id_to_codec`
/// in `hls_segment`).
fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut start: Option<usize> = None;
    let mut i = 0;
    while i + 2 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(s) = start {
                nals.push(&data[s..i]);
            }
            i += 3;
            start = Some(i);
            continue;
        }
        if i + 3 < data.len()
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1
        {
            if let Some(s) = start {
                nals.push(&data[s..i]);
            }
            i += 4;
            start = Some(i);
            continue;
        }
        i += 1;
    }
    if let Some(s) = start {
        nals.push(&data[s..]);
    }
    nals
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
        let tags = match pkt.codec {
            Codec::H264 | Codec::H265 => self.mux_video(&pkt)?,
            Codec::Aac => self.mux_audio(&pkt)?,
            other => {
                metric_pipeline_error!("flv_mux.unsupported_codec");
                tracing::warn!(
                    stream = %self.stream_id,
                    codec = ?other,
                    "FlvMux received unsupported codec; dropping packet"
                );
                return Ok(vec![]);
            }
        };
        Ok(tags)
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use livestream_codec::NalData;
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
                // Tag timestamp is DTS (900), not PTS (1000).
                assert_eq!(*timestamp, 900);
                assert!(*is_keyframe);
                // First byte should be FrameType=1(keyframe) | CodecID=7(AVC) = 0x17
                assert_eq!(payload[0], 0x17);
                // Second byte should be AVCPacketType=1 (NAL unit)
                assert_eq!(payload[1], 0x01);
                // Next three bytes: CompositionTime = PTS - DTS = 100
                assert_eq!(&payload[2..5], &[0x00u8, 0x00, 0x64]);
            }
            _ => panic!("expected video tag"),
        }
    }

    #[test]
    fn mux_video_uses_pts_when_dts_missing() {
        let mux = make_mux();
        let mut pkt =
            EncodedPacket::new_video_keyframe(&[0x00, 0x00, 0x01, 0x65, 0x88][..], 1000, 900, 0);
        pkt.dts_ms = None;
        let tags = mux.mux_video(&pkt).unwrap();
        assert_eq!(tags.len(), 1);
        match &tags[0] {
            FlvTag::Video {
                timestamp, payload, ..
            } => {
                // Falls back to PTS; CompositionTime becomes 0.
                assert_eq!(*timestamp, 1000);
                assert_eq!(&payload[2..5], &[0x00u8, 0x00, 0x00]);
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
            data: NalData::AnnexB(Bytes::from_static(&[0x00])),
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

    // Default (Annex B source, e.g. RTMP): annex_b_to_avcc is a pass-through —
    // ffmpeg's FLV demuxer detects Annex B in the tags itself.

    #[test]
    fn annex_b_to_avcc_passthrough() {
        let mux = make_mux();
        let input = &[0x00, 0x00, 0x01, 0x65, 0x88];
        let result = mux.annex_b_to_avcc(input);
        assert_eq!(&result[..], input);
    }

    #[test]
    fn annex_b_to_avcc_empty() {
        let mux = make_mux();
        assert!(mux.annex_b_to_avcc(&[]).is_empty());
    }

    #[test]
    fn avcc_stream_converts_media_nals() {
        let mux = make_mux();
        // avcC sequence header (configurationVersion=0x01) arms the
        // length-prefixed framing for this stream.
        let header = EncodedPacket {
            codec: Codec::H264,
            stream_index: 0,
            data: NalData::AnnexB(Bytes::new()),
            pts_ms: None,
            dts_ms: None,
            is_keyframe: true,
            is_sequence_header: true,
            is_script_data: false,
            extradata: Some(Bytes::from_static(&[0x01, 0x64, 0x00, 0x2C, 0xFF, 0xE1])),
        };
        let _ = mux.mux_video(&header).unwrap();

        // Media NALs are now length-prefixed instead of Annex B.
        let input = &[
            0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01,
        ];
        let result = mux.annex_b_to_avcc(input);
        assert_eq!(
            &result[..],
            &[
                0x00, 0x00, 0x00, 0x02, 0x65, 0x88, 0x00, 0x00, 0x00, 0x02, 0x06, 0x01
            ]
        );
    }

    #[test]
    fn split_annexb_units() {
        let data = &[
            0x00, 0x00, 0x00, 0x01, 0x67, 0x01, 0x00, 0x00, 0x00, 0x01, 0x68, 0x02, 0x00, 0x00,
            0x01, 0x65,
        ];
        let nals = split_annexb(data);
        assert_eq!(nals, vec![&data[4..6], &data[10..12], &data[15..16]]);
    }
}
