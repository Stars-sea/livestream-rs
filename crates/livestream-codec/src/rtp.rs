//! RTP packet type and header parsing.
//!
//! `RtpPacket` bridges `RtspSource` (which produces raw RTP frames) and
//! `RtpDepackProcessor` (which feeds them to FFmpeg's RTP demuxer).

use bytes::Bytes;
use livestream_core::types::{CodecParams, MediaPacket};

/// A complete RTP packet (12-byte header + payload).
///
/// Carried from `RtspSource` to `RtpDepackProcessor` through the pipeline.
#[derive(Clone, Debug)]
pub struct RtpPacket {
    /// RTP payload type as negotiated in SDP (e.g., 96 for H.264).
    pub payload_type: u8,
    /// RTSP interleaved channel ID (e.g., 0 for video, 2 for audio).
    pub channel: u8,
    /// RTP timestamp extracted from the 12-byte RTP header.
    pub rtp_timestamp: u32,
    /// RTP header marker bit (for video: indicates last packet of a frame).
    pub marker: bool,
    /// Complete RTP packet (12-byte header + payload) for FFmpeg demuxer consumption.
    pub data: Bytes,
}

impl RtpPacket {
    /// Build an RTP packet from its raw components.
    ///
    /// Reconstructs the 12-byte RTP header from the parsed fields and
    /// prepends it to the payload.
    pub fn new(
        payload_type: u8,
        channel: u8,
        rtp_timestamp: u32,
        marker: bool,
        sequence_number: u16,
        ssrc: u32,
        payload: &[u8],
    ) -> Self {
        let mut data = Vec::with_capacity(12 + payload.len());

        // RTP header (RFC 3550 §5.1):
        //  0                   1                   2                   3
        //  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
        // +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
        // |V=2|P|X|  CC   |M|     PT      |       sequence number         |
        // +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
        // |                           timestamp                           |
        // +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
        // |           synchronization source (SSRC) identifier            |
        // +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+

        let first_byte: u8 = 0x80; // V=2, P=0, X=0, CC=0
        data.push(first_byte);

        let marker_pt: u8 = if marker {
            0x80 | (payload_type & 0x7F)
        } else {
            payload_type & 0x7F
        };
        data.push(marker_pt);

        data.extend_from_slice(&sequence_number.to_be_bytes());
        data.extend_from_slice(&rtp_timestamp.to_be_bytes());
        data.extend_from_slice(&ssrc.to_be_bytes());
        data.extend_from_slice(payload);

        Self {
            payload_type,
            channel,
            rtp_timestamp,
            marker,
            data: Bytes::from(data),
        }
    }
}

/// Parse a 12-byte RTP header from a raw buffer.
///
/// Parse a 12-byte RTP header. Returns the parsed fields + the payload slice.
///
/// Returns `None` if the buffer is shorter than 12 bytes.
pub fn parse_rtp_header(data: &[u8]) -> Option<ParsedRtpHeader<'_>> {
    if data.len() < 12 {
        return None;
    }

    Some(ParsedRtpHeader {
        payload_type: data[1] & 0x7F,
        marker: (data[1] & 0x80) != 0,
        sequence_number: u16::from_be_bytes([data[2], data[3]]),
        rtp_timestamp: u32::from_be_bytes([data[4], data[5], data[6], data[7]]),
        ssrc: u32::from_be_bytes([data[8], data[9], data[10], data[11]]),
        payload: &data[12..],
    })
}

/// Parsed fields from a 12-byte RTP header.
#[derive(Debug, Clone, Copy)]
pub struct ParsedRtpHeader<'a> {
    pub payload_type: u8,
    pub marker: bool,
    pub sequence_number: u16,
    pub rtp_timestamp: u32,
    pub ssrc: u32,
    pub payload: &'a [u8],
}

// ── MediaPacket impl ──

impl MediaPacket for RtpPacket {
    fn codec_params(&self) -> &[CodecParams] {
        // RTP-level type doesn't know the codec — the depacketizer determines it.
        &[]
    }

    fn is_keyframe(&self) -> bool {
        // Unknown at this layer — the depacketizer determines keyframe status.
        false
    }

    fn byte_size(&self) -> usize {
        self.data.len()
    }

    fn timestamp(&self) -> Option<std::time::Duration> {
        // Raw RTP timestamp can't be converted to wall-clock duration
        // without the clock rate from CodecParams.
        None
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_rtp_header() {
        let mut data = vec![0u8; 15];
        data[0] = 0x80; // V=2
        data[1] = 0x80 | 96; // M=1, PT=96
        data[2..4].copy_from_slice(&42u16.to_be_bytes());
        data[4..8].copy_from_slice(&90000u32.to_be_bytes());
        data[8..12].copy_from_slice(&0x12345678u32.to_be_bytes());
        data[12..15].copy_from_slice(&[0xAA, 0xBB, 0xCC]);

        let hdr = parse_rtp_header(&data).unwrap();
        assert_eq!(hdr.payload_type, 96);
        assert!(hdr.marker);
        assert_eq!(hdr.sequence_number, 42);
        assert_eq!(hdr.rtp_timestamp, 90000);
        assert_eq!(hdr.ssrc, 0x12345678);
        assert_eq!(hdr.payload, &[0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn parse_too_short() {
        assert!(parse_rtp_header(&[0u8; 8]).is_none());
    }

    #[test]
    fn parse_no_marker() {
        let mut data = vec![0u8; 12];
        data[0] = 0x80; // V=2
        data[1] = 96; // M=0, PT=96

        let hdr = parse_rtp_header(&data).unwrap();
        assert_eq!(hdr.payload_type, 96);
        assert!(!hdr.marker);
    }

    #[test]
    fn build_rtp_packet() {
        let pkt = RtpPacket::new(96, 0, 90000, true, 42, 0x12345678, &[0xAA, 0xBB]);

        assert_eq!(pkt.payload_type, 96);
        assert_eq!(pkt.channel, 0);
        assert_eq!(pkt.rtp_timestamp, 90000);
        assert!(pkt.marker);
        assert_eq!(pkt.data.len(), 14); // 12 header + 2 payload
        assert_eq!(pkt.byte_size(), 14);

        // Verify header bytes
        assert_eq!(pkt.data[0], 0x80); // V=2
        assert_eq!(pkt.data[1], 0x80 | 96); // M=1, PT=96
        assert_eq!(pkt.data[4..8], 90000u32.to_be_bytes());
        assert_eq!(pkt.data[12], 0xAA);
        assert_eq!(pkt.data[13], 0xBB);
    }

    #[test]
    fn media_packet_trait() {
        let pkt = RtpPacket::new(96, 0, 90000, false, 1, 0, &[]);
        assert!(pkt.codec_params().is_empty());
        assert!(!pkt.is_keyframe());
        assert_eq!(pkt.byte_size(), 12);
        assert!(pkt.timestamp().is_none());
    }
}
