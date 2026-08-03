//! FLV tag types: Audio, Video, ScriptData.
//!
//! Pure Rust — no FFmpeg dependency.

use anyhow::Result;
use bytes::Bytes;
use rml_rtmp::rml_amf0::Amf0Value;
use rml_rtmp::sessions::StreamMetadata;

use std::io::Cursor;

/// An FLV tag representing one frame of audio, video, or script data.
#[derive(Clone, Debug)]
pub enum FlvTag {
    Audio {
        timestamp: u32,
        payload: Bytes,
    },
    Video {
        timestamp: u32,
        payload: Bytes,
        is_keyframe: bool,
    },
    ScriptData(StreamMetadata),
}

const FLV_AUDIO_CODEC_AAC: u8 = 10;
const FLV_VIDEO_CODEC_AVC: u8 = 7;
const FLV_VIDEO_CODEC_HEVC: u8 = 12;
const FLV_PACKET_TYPE_SEQ_HEADER: u8 = 0;

impl FlvTag {
    pub fn audio(timestamp: u32, payload: Bytes) -> Self {
        Self::Audio { timestamp, payload }
    }

    pub fn video(timestamp: u32, payload: Bytes) -> Self {
        Self::Video {
            timestamp,
            is_keyframe: is_video_keyframe(&payload),
            payload,
        }
    }

    pub fn script_data(metadata: StreamMetadata) -> Self {
        Self::ScriptData(metadata)
    }

    pub fn payload_size(&self) -> usize {
        match self {
            Self::Audio { payload, .. } => payload.len(),
            Self::Video { payload, .. } => payload.len(),
            Self::ScriptData(_) => 0,
        }
    }

    /// Whether this tag is a codec sequence header.
    pub fn is_sequence_header(&self) -> bool {
        match self {
            Self::Audio { payload, .. } => is_audio_seq_header(payload),
            Self::Video { payload, .. } => is_video_seq_header(payload),
            Self::ScriptData(_) => true,
        }
    }
}

fn is_audio_seq_header(payload: &[u8]) -> bool {
    if payload.is_empty() {
        return false;
    }
    let sound_format = payload[0] >> 4;
    payload.len() >= 2
        && sound_format == FLV_AUDIO_CODEC_AAC
        && payload[1] == FLV_PACKET_TYPE_SEQ_HEADER
}

fn is_video_seq_header(payload: &[u8]) -> bool {
    if payload.is_empty() {
        return false;
    }
    let first_byte = payload[0];
    // Enhanced FLV (E-RTMP v1) video tag header: byte 0 is
    // IsExHeader(bit 7) | FrameType(bits 6-4) | PacketType(bits 3-0),
    // followed by the 4-byte FourCC. Require at least 2 bytes so a
    // truncated ex-header payload cannot be misclassified.
    let is_ex_header = (first_byte & 0x80) != 0;
    if is_ex_header {
        payload.len() >= 2 && (first_byte & 0x0f) == FLV_PACKET_TYPE_SEQ_HEADER
    } else {
        let codec_id = first_byte & 0x0f;
        payload.len() >= 2
            && (codec_id == FLV_VIDEO_CODEC_AVC || codec_id == FLV_VIDEO_CODEC_HEVC)
            && payload[1] == FLV_PACKET_TYPE_SEQ_HEADER
    }
}

/// Parse `onMetaData` from a script data tag payload.
pub(super) fn parse_script_data_metadata(payload: Bytes) -> Result<StreamMetadata> {
    let mut cursor = Cursor::new(payload);
    let mut values = rml_rtmp::rml_amf0::deserialize(&mut cursor)?;

    if values.len() < 2 {
        anyhow::bail!("ScriptData payload is missing required AMF values");
    }

    match &values[0] {
        Amf0Value::Utf8String(name) if name == "onMetaData" => {}
        _ => anyhow::bail!("ScriptData event is not onMetaData"),
    }

    let object = values.remove(1);
    let mut metadata = StreamMetadata::new();
    if let Some(properties) = object.get_object_properties() {
        metadata.apply_metadata_values(properties);
    }

    Ok(metadata)
}

fn is_video_keyframe(payload: &Bytes) -> bool {
    if let Some(&first_byte) = payload.first() {
        // FrameType is bits 4-6 (mask 0x70) for both classic and enhanced
        // (E-RTMP v1) video tag headers — the IsExHeader flag (bit 7) is
        // orthogonal. KeyFrame == 1.
        let frame_type = (first_byte & 0x70) >> 4;
        return frame_type == 1;
    }
    false
}

// ── MediaPacket impl (needed by pipeline Sink trait) ──

use livestream_core::types::{CodecParams, MediaPacket};
use std::time::Duration;

impl MediaPacket for FlvTag {
    fn codec_params(&self) -> &[CodecParams] {
        &[]
    }

    fn is_keyframe(&self) -> bool {
        match self {
            Self::Video { is_keyframe, .. } => *is_keyframe,
            _ => false,
        }
    }

    fn byte_size(&self) -> usize {
        self.payload_size()
    }

    fn timestamp(&self) -> Option<Duration> {
        match self {
            Self::Audio { timestamp, .. } | Self::Video { timestamp, .. } => {
                Some(Duration::from_millis(*timestamp as u64))
            }
            Self::ScriptData(_) => None,
        }
    }
}
