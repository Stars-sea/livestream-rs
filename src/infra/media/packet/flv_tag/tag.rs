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
        let frame_type = (first_byte & 0xF0) >> 4;
        return frame_type == 1;
    }
    false
}
