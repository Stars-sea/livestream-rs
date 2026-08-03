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

/// Parse an RTP header from a raw buffer.
///
/// Handles the full RFC 3550 §5.1 header layout: the fixed 12-byte header,
/// the CSRC list (4 bytes per entry when CC > 0), the optional extension
/// header (4 bytes + `extension_len * 4` when X = 1), and strips trailing
/// padding octets when P = 1.
///
/// Returns the parsed fields + the payload slice (header and padding
/// excluded). Returns `None` if the buffer is too short, if the version is
/// not 2, or if the header/padding lengths declared by the flags exceed the
/// available bytes.
pub fn parse_rtp_header(data: &[u8]) -> Option<ParsedRtpHeader<'_>> {
    if data.len() < 12 {
        return None;
    }

    // First byte: V(2 bits) | P(1 bit) | X(1 bit) | CC(4 bits).
    let first = data[0];
    if first >> 6 != 2 {
        // RFC 3550: only RTP version 2 is in use; reject others.
        return None;
    }
    let has_padding = (first & 0x20) != 0;
    let has_extension = (first & 0x10) != 0;
    let csrc_count = (first & 0x0F) as usize;

    // Fixed header + CSRC list + optional extension header.
    let mut header_len = 12 + csrc_count * 4;
    if has_extension {
        // Extension header: 16-bit profile + 16-bit length (in 32-bit words,
        // excluding the 4-octet extension header itself).
        let ext_start = header_len;
        let ext_end = ext_start + 4;
        if data.len() < ext_end {
            return None;
        }
        let extension_len = u16::from_be_bytes([data[ext_start + 2], data[ext_start + 3]]) as usize;
        header_len = ext_end + extension_len * 4;
    }

    if data.len() < header_len {
        return None;
    }

    let mut payload = &data[header_len..];

    // RFC 3550 §5.1: when P = 1 the last payload byte holds the count of
    // padding octets (including that byte itself).
    if has_padding {
        let &pad_len = payload.last()?;
        let pad_len = pad_len as usize;
        if pad_len == 0 || pad_len > payload.len() {
            return None;
        }
        payload = &payload[..payload.len() - pad_len];
    }

    Some(ParsedRtpHeader {
        payload_type: data[1] & 0x7F,
        marker: (data[1] & 0x80) != 0,
        sequence_number: u16::from_be_bytes([data[2], data[3]]),
        rtp_timestamp: u32::from_be_bytes([data[4], data[5], data[6], data[7]]),
        ssrc: u32::from_be_bytes([data[8], data[9], data[10], data[11]]),
        payload,
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
    fn parse_with_csrc_list() {
        // V=2, CC=2 -> header = 12 + 2*4 = 20 bytes.
        let mut data = vec![0u8; 24];
        data[0] = 0x80 | 0x02; // V=2, CC=2
        data[1] = 96;
        data[8..12].copy_from_slice(&0x11111111u32.to_be_bytes()); // SSRC
        data[12..16].copy_from_slice(&0x22222222u32.to_be_bytes()); // CSRC[0]
        data[16..20].copy_from_slice(&0x33333333u32.to_be_bytes()); // CSRC[1]
        data[20..24].copy_from_slice(&[1, 2, 3, 4]);

        let hdr = parse_rtp_header(&data).unwrap();
        assert_eq!(hdr.ssrc, 0x11111111);
        assert_eq!(hdr.payload, &[1, 2, 3, 4]);
    }

    #[test]
    fn parse_with_extension() {
        // V=2, X=1, CC=0. Extension header: 16-bit profile + 16-bit length
        // (1 word of extension data) -> header = 12 + 4 + 4 = 20 bytes.
        let mut data = vec![0u8; 23];
        data[0] = 0x80 | 0x10; // V=2, X=1
        data[1] = 96;
        data[12..14].copy_from_slice(&0xBEDEu16.to_be_bytes()); // profile
        data[14..16].copy_from_slice(&1u16.to_be_bytes()); // extension length (words)
        data[16..20].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // extension data
        data[20..23].copy_from_slice(&[7, 8, 9]);

        let hdr = parse_rtp_header(&data).unwrap();
        assert_eq!(hdr.payload, &[7, 8, 9]);
    }

    #[test]
    fn parse_with_padding() {
        // V=2, P=1. Last payload byte holds the pad count (incl. itself).
        let mut data = vec![0u8; 18];
        data[0] = 0x80 | 0x20; // V=2, P=1
        data[1] = 96;
        data[12..15].copy_from_slice(&[0xAA, 0xBB, 0xCC]); // real payload
        data[15] = 0x00; // padding byte
        data[16] = 0x00; // padding byte
        data[17] = 3; // 3 padding octets (last byte holds the count)

        let hdr = parse_rtp_header(&data).unwrap();
        assert_eq!(hdr.payload, &[0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn parse_with_csrc_extension_and_padding() {
        // V=2, P=1, X=1, CC=1 -> header = 12 + 4 + 4 + 4 = 24 bytes.
        let mut data = vec![0u8; 29];
        data[0] = 0x80 | 0x20 | 0x10 | 0x01; // V=2, P=1, X=1, CC=1
        data[1] = 96;
        data[8..12].copy_from_slice(&0xABCDEF01u32.to_be_bytes()); // SSRC
        data[12..16].copy_from_slice(&0x12345678u32.to_be_bytes()); // CSRC[0]
        data[16..18].copy_from_slice(&0xBEDEu16.to_be_bytes()); // profile
        data[18..20].copy_from_slice(&1u16.to_be_bytes()); // extension length (words)
        data[20..24].copy_from_slice(&[1, 2, 3, 4]); // extension data
        data[24..27].copy_from_slice(&[0xAA, 0xBB, 0xCC]); // real payload
        data[27] = 0x00; // padding byte
        data[28] = 2; // 2 padding octets (last byte holds the count)

        let hdr = parse_rtp_header(&data).unwrap();
        assert_eq!(hdr.ssrc, 0xABCDEF01);
        assert_eq!(hdr.payload, &[0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn parse_rejects_wrong_version() {
        let mut data = vec![0u8; 12];
        data[0] = 0x00; // V=0
        assert!(parse_rtp_header(&data).is_none());

        data[0] = 0x80; // V=2
        assert!(parse_rtp_header(&data).is_some());

        data[0] = 0xC0; // V=3
        assert!(parse_rtp_header(&data).is_none());
    }

    #[test]
    fn parse_rejects_truncated_header() {
        // CC=3 declares 12 more header bytes than are present.
        let mut data = vec![0u8; 16];
        data[0] = 0x80 | 0x03; // V=2, CC=3
        assert!(parse_rtp_header(&data).is_none());

        // X=1 with an extension length that overruns the buffer.
        let mut data = vec![0u8; 18];
        data[0] = 0x80 | 0x10; // V=2, X=1
        data[14..16].copy_from_slice(&4u16.to_be_bytes()); // claims 16 extension bytes
        assert!(parse_rtp_header(&data).is_none());
    }

    #[test]
    fn parse_rejects_invalid_padding() {
        // Pad count larger than the available payload.
        let mut data = vec![0u8; 15];
        data[0] = 0x80 | 0x20; // V=2, P=1
        data[12] = 0xAA;
        data[13] = 0xBB;
        data[14] = 0x04; // claims 4 padding octets, only 3 payload bytes
        assert!(parse_rtp_header(&data).is_none());

        // Pad count of 0 is invalid (count includes the count byte itself).
        let mut data = vec![0u8; 13];
        data[0] = 0x80 | 0x20; // V=2, P=1
        data[12] = 0x00;
        assert!(parse_rtp_header(&data).is_none());
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
