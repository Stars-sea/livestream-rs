use bytes::Bytes;
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
        FlvTag::Audio { timestamp, payload }
    }

    pub fn video(timestamp: u32, payload: Bytes) -> Self {
        let is_keyframe = is_video_keyframe(&payload);
        FlvTag::Video {
            timestamp,
            payload,
            is_keyframe,
        }
    }

    pub fn script_data(metadata: StreamMetadata) -> Self {
        FlvTag::ScriptData(metadata)
    }
}

fn is_video_keyframe(payload: &Bytes) -> bool {
    if let Some(&first_byte) = payload.first() {
        let frame_type = (first_byte & 0xF0) >> 4;
        return frame_type == 1;
    }
    false
}
