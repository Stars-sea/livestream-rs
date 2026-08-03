//! SDP parser for RTSP ANNOUNCE requests.
//!
//! Parses media descriptions, extracts codec parameters, and produces
//! an FFmpeg-compatible SDP string for the RTP demuxer.

use anyhow::Result;
use base64::Engine;
use livestream_core::types::{Codec, CodecParams, MediaType};

/// Parsed SDP from an RTSP ANNOUNCE request.
pub struct ParsedSdp {
    pub codec_params: Vec<CodecParams>,
    /// The original raw SDP body.
    raw_sdp: String,
}

impl ParsedSdp {
    /// Return the raw SDP body for use by FFmpeg's RTP demuxer.
    ///
    /// The SDP is returned as-is — `RtpDemuxContext` feeds it to FFmpeg
    /// via `avformat_open_input()` with a custom AVIO for RTP data.
    pub fn raw_sdp(&self) -> &str {
        &self.raw_sdp
    }
}

pub fn parse_sdp(sdp_body: &str) -> Result<ParsedSdp> {
    let mut codec_params = Vec::new();

    for line in sdp_body.lines() {
        let line = line.trim();
        if !line.starts_with("m=") {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 {
            continue;
        }

        let media_str = parts[0];
        let payload_type = parts[3];

        let media_type = match media_str {
            "m=video" => MediaType::Video,
            "m=audio" => MediaType::Audio,
            _ => continue,
        };

        let rtpmap = find_attr(sdp_body, &format!("a=rtpmap:{}", payload_type));
        let (codec, clock_rate) = match rtpmap {
            Some(ref map) => parse_rtpmap(map)?,
            None if media_type == MediaType::Video && payload_type == "26" => {
                // RFC 3551 static payload type 26 = JPEG; no rtpmap attribute
                // needed (ffmpeg's RTSP muxer omits it for MJPEG sources).
                (Codec::Mjpeg, 90_000)
            }
            None => continue,
        };

        let fmtp = find_attr(sdp_body, &format!("a=fmtp:{}", payload_type));
        let extradata = parse_fmtp_config(&fmtp.unwrap_or_default(), &codec);

        codec_params.push(CodecParams {
            codec,
            media_type,
            clock_rate,
            extradata,
        });
    }

    if codec_params.is_empty() {
        anyhow::bail!("No valid media descriptions found in SDP");
    }

    Ok(ParsedSdp {
        codec_params,
        raw_sdp: sdp_body.to_string(),
    })
}

fn find_attr(sdp: &str, prefix: &str) -> Option<String> {
    for line in sdp.lines() {
        let line = line.trim();
        // Exact prefix match — must be followed by whitespace, end-of-line,
        // or nothing. Prevents "a=rtpmap:96" from matching "a=rtpmap:96-profile-level-id".
        if let Some(rest) = line.strip_prefix(prefix)
            && (rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t'))
        {
            return Some(rest.trim().to_string());
        }
    }
    None
}

fn parse_rtpmap(rtpmap: &str) -> Result<(Codec, u32)> {
    // rtpmap may be "PT CODEC/RATE" or just "CODEC/RATE"
    let codec_part = rtpmap.split_whitespace().last().unwrap_or(rtpmap);
    let codec_parts: Vec<&str> = codec_part.split('/').collect();
    if codec_parts.len() < 2 {
        anyhow::bail!("Invalid codec/clock-rate: {}", rtpmap);
    }
    let codec_name = codec_parts[0].to_lowercase();
    let clock_rate: u32 = codec_parts[1].parse()?;

    let codec = match codec_name.as_str() {
        "h264" | "h264-rs" => Codec::H264,
        "h265" | "hevc" => Codec::H265,
        "mpeg4-generic" | "aac" | "mpeg4" => Codec::Aac,
        "opus" => Codec::Opus,
        "mp3" | "mpa" | "mpeg" => Codec::Mp3,
        "jpeg" | "mjpeg" => Codec::Mjpeg,
        _ => anyhow::bail!("Unsupported codec: {}", codec_name),
    };
    Ok((codec, clock_rate))
}

fn decode_base64_part(part: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(part.trim())
        .ok()
}

fn parse_fmtp_config(fmtp: &str, codec: &Codec) -> Option<bytes::Bytes> {
    match codec {
        Codec::H264 => {
            let sets = extract_param(fmtp, "sprop-parameter-sets")?;
            let buf: Vec<u8> = sets
                .split(',')
                .filter_map(decode_base64_part)
                .flatten()
                .collect();
            if buf.is_empty() {
                None
            } else {
                Some(bytes::Bytes::from(buf))
            }
        }
        Codec::Aac => {
            let config = extract_param(fmtp, "config")?;
            hex_decode(config).ok().map(bytes::Bytes::from)
        }
        _ => None,
    }
}

fn extract_param<'a>(fmtp: &'a str, key: &str) -> Option<&'a str> {
    for part in fmtp.split(';') {
        let part = part.trim();
        if let Some(idx) = part.find('=')
            && part[..idx].trim() == key
        {
            return Some(part[idx + 1..].trim());
        }
    }
    None
}

/// Hex string to bytes (e.g., "1210" → [0x12, 0x10]).
///
/// Kept as a small private function — the `hex` crate is not a workspace
/// dependency and adding it for 5 lines is not justified.
fn hex_decode(input: &str) -> Result<Vec<u8>> {
    let input = input.trim();
    if !input.len().is_multiple_of(2) {
        anyhow::bail!("Hex string has odd length");
    }
    let mut bytes = Vec::with_capacity(input.len() / 2);
    for i in (0..input.len()).step_by(2) {
        let byte = u8::from_str_radix(&input[i..i + 2], 16)?;
        bytes.push(byte);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_video_rtpmap() {
        let (codec, rate) = parse_rtpmap("96 H264/90000").unwrap();
        assert_eq!(codec, Codec::H264);
        assert_eq!(rate, 90000);
    }

    #[test]
    fn parse_jpeg_rtpmap() {
        let (codec, rate) = parse_rtpmap("26 JPEG/90000").unwrap();
        assert_eq!(codec, Codec::Mjpeg);
        assert_eq!(rate, 90000);
    }

    #[test]
    fn parse_static_payload_type_26_without_rtpmap() {
        // ffmpeg's RTSP muxer pushes MJPEG with a bare `m=video 0 RTP/AVP 26`
        // and no rtpmap attribute — RFC 3551 static payload type.
        let sdp = "\
            v=0\r\n\
            m=video 0 RTP/AVP 26\r\n";
        let result = parse_sdp(sdp).unwrap();
        assert_eq!(result.codec_params.len(), 1);
        assert_eq!(result.codec_params[0].codec, Codec::Mjpeg);
        assert_eq!(result.codec_params[0].media_type, MediaType::Video);
        assert_eq!(result.codec_params[0].clock_rate, 90000);
    }

    #[test]
    fn parse_dynamic_payload_type_96_jpeg_rtpmap() {
        let sdp = "\
            v=0\r\n\
            m=video 0 RTP/AVP 96\r\n\
            a=rtpmap:96 JPEG/90000\r\n";
        let result = parse_sdp(sdp).unwrap();
        assert_eq!(result.codec_params.len(), 1);
        assert_eq!(result.codec_params[0].codec, Codec::Mjpeg);
        assert_eq!(result.codec_params[0].clock_rate, 90000);
    }

    #[test]
    fn base64_decode_sps() {
        let sps_b64 = "Z0IACg=="; // "gB\x00\n" = [0x67, 0x42, 0x00, 0x0A]
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(sps_b64)
            .unwrap();
        assert_eq!(decoded, vec![0x67, 0x42, 0x00, 0x0A]);
    }

    #[test]
    fn parse_simple_sdp() {
        let sdp = "\
            m=video 0 RTP/AVP 96\r\n\
            a=rtpmap:96 H264/90000\r\n\
            m=audio 0 RTP/AVP 97\r\n\
            a=rtpmap:97 mpeg4-generic/44100/2\r\n\
            a=fmtp:97 config=1210\r\n";
        let result = parse_sdp(sdp).unwrap();
        assert_eq!(result.codec_params.len(), 2);
        assert!(result.codec_params[0].is_video());
        assert_eq!(result.codec_params[0].codec, Codec::H264);
        assert_eq!(result.codec_params[1].codec, Codec::Aac);
    }

    #[test]
    fn raw_sdp_preserved() {
        let sdp = "v=0\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\n";
        let result = parse_sdp(sdp).unwrap();
        assert_eq!(result.raw_sdp(), sdp);
    }

    #[test]
    fn hex_decode_correct() {
        let bytes = hex_decode("1210").unwrap();
        assert_eq!(bytes, vec![0x12, 0x10]);
    }

    #[test]
    fn hex_decode_odd_length() {
        assert!(hex_decode("121").is_err());
    }

    #[test]
    fn sprop_parameter_sets_base64() {
        let fmtp = "sprop-parameter-sets=Z0IACg==,aM4G4g==";
        let config = parse_fmtp_config(fmtp, &Codec::H264).unwrap();
        assert!(!config.is_empty());
    }
}
