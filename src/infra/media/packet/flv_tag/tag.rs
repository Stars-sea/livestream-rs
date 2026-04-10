use std::io::Cursor;

use anyhow::Result;
use bytes::Bytes;
use rml_rtmp::rml_amf0::Amf0Value;
use rml_rtmp::sessions::StreamMetadata;

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

    pub fn is_sequence_header(&self) -> bool {
        match self {
            Self::Audio { payload, .. } => {
                if payload.is_empty() {
                    return false;
                }
                let sound_format = payload[0] >> 4;
                payload.len() >= 2
                    && sound_format == FLV_AUDIO_CODEC_AAC
                    && payload[1] == FLV_PACKET_TYPE_SEQ_HEADER
            }
            Self::Video { payload, .. } => {
                if payload.is_empty() {
                    return false;
                }
                let first_byte = payload[0];
                let is_ex_header = (first_byte & 0x80) != 0;

                if is_ex_header {
                    // Enhanced RTMP (v1 or v2)
                    let packet_type = first_byte & 0x0f;
                    packet_type == FLV_PACKET_TYPE_SEQ_HEADER
                } else {
                    // Standard FLV
                    let codec_id = first_byte & 0x0f;
                    payload.len() >= 2
                        && (codec_id == FLV_VIDEO_CODEC_AVC || codec_id == FLV_VIDEO_CODEC_HEVC)
                        && payload[1] == FLV_PACKET_TYPE_SEQ_HEADER
                }
            }
            Self::ScriptData(_) => true,
        }
    }
}

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
        // According to both standard FLV and Enhanced FLV:
        // FrameType is bits 4-6 (mask 0x70)
        let frame_type = (first_byte & 0x70) >> 4;
        return frame_type == 1;
    }
    false
}
