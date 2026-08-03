use bytes::Bytes;
use std::fmt;
use std::time::Duration;

// ── Codec ──

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    H264,
    H265,
    Aac,
    Mp3,
    Opus,
    Av1,
    Mjpeg,
}

impl Codec {
    pub fn is_supported(&self) -> bool {
        matches!(self, Self::H264 | Self::Aac)
    }
}

// ── Protocol ──

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Protocol {
    Rtmp,
    Rtsp,
    Hls,
    HttpFlv,
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rtmp => write!(f, "RTMP"),
            Self::Rtsp => write!(f, "RTSP"),
            Self::Hls => write!(f, "HLS"),
            Self::HttpFlv => write!(f, "HTTP-FLV"),
        }
    }
}

// ── MediaType ──

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaType {
    Video,
    Audio,
}

// ── CodecParams ──

/// Describes the codec configuration of a media stream.
#[derive(Clone, Debug)]
pub struct CodecParams {
    pub codec: Codec,
    pub media_type: MediaType,
    /// Clock rate in Hz (e.g., 90000 for MPEG-TS video, 44100 for AAC audio).
    pub clock_rate: u32,
    /// Codec-specific initialization data (SPS+PPS for H.264, ASC for AAC).
    pub extradata: Option<Bytes>,
}

impl CodecParams {
    pub fn new_video(codec: Codec, clock_rate: u32, extradata: Option<Bytes>) -> Self {
        Self {
            codec,
            media_type: MediaType::Video,
            clock_rate,
            extradata,
        }
    }

    pub fn new_audio(codec: Codec, clock_rate: u32, extradata: Option<Bytes>) -> Self {
        Self {
            codec,
            media_type: MediaType::Audio,
            clock_rate,
            extradata,
        }
    }

    pub fn is_video(&self) -> bool {
        matches!(self.media_type, MediaType::Video)
    }

    pub fn is_audio(&self) -> bool {
        matches!(self.media_type, MediaType::Audio)
    }
}

// ── MediaPacket ──

/// Trait for types that flow through the pipeline.
pub trait MediaPacket: Send + Sync + 'static {
    /// Codec parameters for this packet.
    fn codec_params(&self) -> &[CodecParams];
    /// Whether this packet is a keyframe (IDR for H.264).
    fn is_keyframe(&self) -> bool;
    /// Approximate byte size of the packet data.
    fn byte_size(&self) -> usize;
    /// Presentation timestamp relative to stream start.
    fn timestamp(&self) -> Option<Duration>;
}
