use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use livestream_core::types::{Codec, CodecParams, MediaPacket};

/// An encoded media packet (compressed frame or sequence header).
///
/// This is the universal data carrier for the pipeline. All Sources produce it;
/// all Processors consume and produce it.
#[derive(Clone, Debug)]
pub struct EncodedPacket {
    /// Codec of this packet's data.
    pub codec: Codec,

    /// Stream index within the source (0 = first video, 1 = first audio, etc.).
    pub stream_index: usize,

    /// Encoded bitstream data (H.264 NAL units, AAC frames, etc.).
    pub data: Bytes,

    /// Presentation timestamp in milliseconds since stream start.
    pub pts_ms: Option<i64>,

    /// Decode timestamp in milliseconds since stream start.
    pub dts_ms: Option<i64>,

    /// Whether this packet is a keyframe (IDR for H.264).
    pub is_keyframe: bool,

    /// Whether this packet is a sequence header (SPS/PPS for H.264, ASC for AAC).
    pub is_sequence_header: bool,

    /// Whether this packet is script data (onMetaData, cue points).
    pub is_script_data: bool,

    /// Codec-specific extradata (SPS+PPS for H.264, AudioSpecificConfig for AAC).
    pub extradata: Option<Bytes>,
}

impl EncodedPacket {
    /// Create from an H.264 keyframe.
    pub fn new_video_keyframe(
        data: impl Into<Bytes>,
        pts_ms: i64,
        dts_ms: i64,
        stream_index: usize,
    ) -> Self {
        Self {
            codec: Codec::H264,
            stream_index,
            data: data.into(),
            pts_ms: Some(pts_ms),
            dts_ms: Some(dts_ms),
            is_keyframe: true,
            is_sequence_header: false,
            is_script_data: false,
            extradata: None,
        }
    }

    /// Create from raw AAC audio data.
    pub fn new_audio(data: impl Into<Bytes>, pts_ms: i64, stream_index: usize) -> Self {
        Self {
            codec: Codec::Aac,
            stream_index,
            data: data.into(),
            pts_ms: Some(pts_ms),
            dts_ms: None,
            is_keyframe: false,
            is_sequence_header: false,
            is_script_data: false,
            extradata: None,
        }
    }

    /// Create an AVC sequence header (SPS + PPS).
    pub fn new_avc_sequence_header(sps: &[u8], pps: &[u8], stream_index: usize) -> Self {
        let mut extradata = Vec::with_capacity(sps.len() + pps.len());
        extradata.extend_from_slice(sps);
        extradata.extend_from_slice(pps);

        Self {
            codec: Codec::H264,
            stream_index,
            data: Bytes::new(),
            pts_ms: None,
            dts_ms: None,
            is_keyframe: true,
            is_sequence_header: true,
            is_script_data: false,
            extradata: Some(Bytes::from(extradata)),
        }
    }

    /// Create an AAC sequence header (AudioSpecificConfig).
    pub fn new_aac_sequence_header(asc: &[u8], stream_index: usize) -> Self {
        Self {
            codec: Codec::Aac,
            stream_index,
            data: Bytes::new(),
            pts_ms: None,
            dts_ms: None,
            is_keyframe: false,
            is_sequence_header: true,
            is_script_data: false,
            extradata: Some(Bytes::copy_from_slice(asc)),
        }
    }

    /// Approximate byte size.
    pub fn byte_size(&self) -> usize {
        self.data.len()
    }

    /// Whether this is a video codec packet.
    pub fn is_video(&self) -> bool {
        matches!(self.codec, Codec::H264 | Codec::H265 | Codec::Av1)
    }

    /// Whether this is an audio codec packet.
    pub fn is_audio(&self) -> bool {
        matches!(self.codec, Codec::Aac | Codec::Opus | Codec::Mp3)
    }
}

impl MediaPacket for EncodedPacket {
    fn codec_params(&self) -> &[CodecParams] {
        &[]
    }

    fn is_keyframe(&self) -> bool {
        self.is_keyframe
    }

    fn byte_size(&self) -> usize {
        self.data.len()
    }

    fn timestamp(&self) -> Option<Duration> {
        self.pts_ms
            .map(|ms| Duration::from_millis(ms.max(0) as u64))
    }
}

// ── TsSegment ──

/// A completed MPEG-TS segment ready for upload.
#[derive(Clone, Debug)]
pub struct TsSegment {
    /// Path to the segment file on disk.
    pub path: PathBuf,

    /// Filename within the HLS playlist (e.g., "segment_0000.ts").
    pub filename: String,

    /// Sequence number in the HLS playlist (monotonically increasing).
    pub sequence: u64,

    /// Media duration of this segment.
    pub duration: Duration,

    /// Whether this is the final segment in the stream.
    pub is_final: bool,
}

impl MediaPacket for TsSegment {
    fn codec_params(&self) -> &[CodecParams] {
        &[]
    }

    fn is_keyframe(&self) -> bool {
        true
    }

    fn byte_size(&self) -> usize {
        0 // on-disk size not tracked in struct
    }

    fn timestamp(&self) -> Option<Duration> {
        self.duration.checked_mul(self.sequence as u32)
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn video_keyframe_has_correct_properties() {
        let data: &[u8] = &[0x00, 0x00, 0x01];
        let pkt = EncodedPacket::new_video_keyframe(data, 1000, 900, 0);
        assert_eq!(pkt.codec, Codec::H264);
        assert!(pkt.is_keyframe);
        assert!(!pkt.is_sequence_header);
        assert!(pkt.is_video());
        assert!(!pkt.is_audio());
        assert_eq!(pkt.timestamp(), Some(Duration::from_millis(1000)));
    }

    #[test]
    fn audio_packet_has_correct_properties() {
        let data: &[u8] = &[0xff, 0xf1];
        let pkt = EncodedPacket::new_audio(data, 500, 1);
        assert_eq!(pkt.codec, Codec::Aac);
        assert!(!pkt.is_keyframe);
        assert!(pkt.is_audio());
        assert!(!pkt.is_video());
    }

    #[test]
    fn avc_sequence_header() {
        let sps = b"\x67\x42\x00";
        let pps = b"\x68\xce\x3c";
        let pkt = EncodedPacket::new_avc_sequence_header(sps, pps, 0);
        assert!(pkt.is_sequence_header);
        assert!(pkt.is_keyframe);
        assert!(pkt.data.is_empty());
        assert_eq!(pkt.extradata.as_ref().map(|b| b.len()), Some(6));
    }

    #[test]
    fn aac_sequence_header() {
        let asc = b"\x12\x10";
        let pkt = EncodedPacket::new_aac_sequence_header(asc, 1);
        assert!(pkt.is_sequence_header);
        assert!(!pkt.is_keyframe);
        assert_eq!(pkt.codec, Codec::Aac);
    }

    #[test]
    fn ts_segment_media_packet_impl() {
        let seg = TsSegment {
            path: "/tmp/segment_0000.ts".into(),
            filename: "segment_0000.ts".into(),
            sequence: 3,
            duration: Duration::from_secs(2),
            is_final: false,
        };
        assert!(seg.is_keyframe());
        assert_eq!(seg.byte_size(), 0);
        assert_eq!(
            seg.timestamp(),
            Some(Duration::from_secs(6)) // 2 * 3
        );
    }
}
