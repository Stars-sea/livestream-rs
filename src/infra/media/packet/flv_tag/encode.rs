use std::collections::HashMap;

use anyhow::Result;
use bytes::{BufMut, Bytes, BytesMut};
use rml_rtmp::rml_amf0::Amf0Value;
use rml_rtmp::sessions::StreamMetadata;

use super::FlvTag;

const FLV_HEADER_LEN: usize = 13;
const FLV_TAG_HEADER_LEN: usize = 11;
const FLV_PREVIOUS_TAG_SIZE_LEN: usize = 4;
const FLV_TAG_TYPE_AUDIO: u8 = 8;
const FLV_TAG_TYPE_VIDEO: u8 = 9;
const FLV_TAG_TYPE_SCRIPT_DATA: u8 = 18;

pub fn encode_flv_header(has_audio: bool, has_video: bool) -> Bytes {
    let mut bytes = BytesMut::with_capacity(FLV_HEADER_LEN);
    bytes.extend_from_slice(b"FLV");
    bytes.put_u8(1);

    let mut flags = 0u8;
    if has_audio {
        flags |= 0b0000_0100;
    }
    if has_video {
        flags |= 0b0000_0001;
    }
    bytes.put_u8(flags);
    bytes.put_u32(9);
    bytes.put_u32(0);
    bytes.freeze()
}

pub fn encode_flv_tag(tag: &FlvTag) -> Result<Bytes> {
    let (tag_type, timestamp, payload) = match tag {
        FlvTag::Audio { timestamp, payload } => (FLV_TAG_TYPE_AUDIO, *timestamp, payload.clone()),
        FlvTag::Video {
            timestamp, payload, ..
        } => (FLV_TAG_TYPE_VIDEO, *timestamp, payload.clone()),
        FlvTag::ScriptData(metadata) => {
            (FLV_TAG_TYPE_SCRIPT_DATA, 0, encode_script_data(metadata)?)
        }
    };

    let payload_len = payload.len();
    let tag_len = FLV_TAG_HEADER_LEN + payload_len + FLV_PREVIOUS_TAG_SIZE_LEN;
    let mut bytes = BytesMut::with_capacity(tag_len);

    bytes.put_u8(tag_type);
    put_u24(&mut bytes, payload_len as u32);
    put_u24(&mut bytes, timestamp & 0x00ff_ffff);
    bytes.put_u8(((timestamp >> 24) & 0xff) as u8);
    put_u24(&mut bytes, 0);
    bytes.extend_from_slice(&payload);
    bytes.put_u32((FLV_TAG_HEADER_LEN + payload_len) as u32);

    Ok(bytes.freeze())
}

fn encode_script_data(metadata: &StreamMetadata) -> Result<Bytes> {
    let values = vec![
        Amf0Value::Utf8String("onMetaData".to_string()),
        Amf0Value::Object(metadata_to_amf_object(metadata)),
    ];
    let encoded = rml_rtmp::rml_amf0::serialize(&values)?;
    Ok(Bytes::from(encoded))
}

fn metadata_to_amf_object(metadata: &StreamMetadata) -> HashMap<String, Amf0Value> {
    let mut properties = HashMap::new();

    if let Some(width) = metadata.video_width {
        properties.insert("width".to_string(), Amf0Value::Number(width as f64));
    }
    if let Some(height) = metadata.video_height {
        properties.insert("height".to_string(), Amf0Value::Number(height as f64));
    }
    if let Some(codec_id) = metadata.video_codec_id {
        properties.insert(
            "videocodecid".to_string(),
            Amf0Value::Number(codec_id as f64),
        );
    }
    if let Some(frame_rate) = metadata.video_frame_rate {
        properties.insert(
            "framerate".to_string(),
            Amf0Value::Number(frame_rate as f64),
        );
    }
    if let Some(video_bitrate_kbps) = metadata.video_bitrate_kbps {
        properties.insert(
            "videodatarate".to_string(),
            Amf0Value::Number(video_bitrate_kbps as f64),
        );
    }
    if let Some(codec_id) = metadata.audio_codec_id {
        properties.insert(
            "audiocodecid".to_string(),
            Amf0Value::Number(codec_id as f64),
        );
    }
    if let Some(audio_bitrate_kbps) = metadata.audio_bitrate_kbps {
        properties.insert(
            "audiodatarate".to_string(),
            Amf0Value::Number(audio_bitrate_kbps as f64),
        );
    }
    if let Some(sample_rate) = metadata.audio_sample_rate {
        properties.insert(
            "audiosamplerate".to_string(),
            Amf0Value::Number(sample_rate as f64),
        );
    }
    if let Some(audio_channels) = metadata.audio_channels {
        properties.insert(
            "audiochannels".to_string(),
            Amf0Value::Number(audio_channels as f64),
        );
    }
    if let Some(audio_is_stereo) = metadata.audio_is_stereo {
        properties.insert("stereo".to_string(), Amf0Value::Boolean(audio_is_stereo));
    }
    if let Some(encoder) = &metadata.encoder {
        properties.insert(
            "encoder".to_string(),
            Amf0Value::Utf8String(encoder.clone()),
        );
    }

    properties
}

fn put_u24(bytes: &mut BytesMut, value: u32) {
    bytes.put_u8(((value >> 16) & 0xff) as u8);
    bytes.put_u8(((value >> 8) & 0xff) as u8);
    bytes.put_u8((value & 0xff) as u8);
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use rml_rtmp::sessions::StreamMetadata;

    use super::{encode_flv_header, encode_flv_tag};
    use crate::infra::media::packet::FlvTag;

    #[test]
    fn encode_header_with_audio_and_video_flags() {
        let header = encode_flv_header(true, true);
        assert_eq!(&header[..3], b"FLV");
        assert_eq!(header[4], 0b0000_0101);
        assert_eq!(&header[5..9], &[0, 0, 0, 9]);
        assert_eq!(&header[9..13], &[0, 0, 0, 0]);
    }

    #[test]
    fn encode_video_tag_with_previous_tag_size() {
        let tag = FlvTag::video(0x01020304, Bytes::from_static(&[0x17, 0x01, 0x00]));
        let encoded = encode_flv_tag(&tag).expect("video tag should encode");

        assert_eq!(encoded[0], 9);
        assert_eq!(&encoded[1..4], &[0, 0, 3]);
        assert_eq!(&encoded[4..7], &[0x02, 0x03, 0x04]);
        assert_eq!(encoded[7], 0x01);
        assert_eq!(&encoded[11..14], &[0x17, 0x01, 0x00]);
        assert_eq!(&encoded[14..18], &[0, 0, 0, 14]);
    }

    #[test]
    fn encode_script_data_tag() {
        let mut metadata = StreamMetadata::new();
        metadata.video_width = Some(1920);
        metadata.encoder = Some("obs".to_string());

        let encoded =
            encode_flv_tag(&FlvTag::script_data(metadata)).expect("metadata should encode");
        assert_eq!(encoded[0], 18);
        assert!(encoded.len() > 15);
    }
}
