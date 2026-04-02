use std::io::Cursor;

use anyhow::Result;
use bytes::Bytes;
use ffmpeg_sys_next::{AVMediaType, AVRational, av_malloc, av_rescale_q};
use rml_rtmp::rml_amf0::Amf0Value;
use rml_rtmp::sessions::StreamMetadata;

use super::Packet;
use crate::infra::media::codec::CodecParamsTrait;
use crate::infra::media::stream::StreamCollection;

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

    pub fn to_packet(self, streams: &dyn StreamCollection) -> Result<Packet> {
        let mapping = FlvStreamMapping::from_streams(streams)?;

        match self {
            FlvTag::Audio { timestamp, payload } => Ok(Self::make_packet(
                payload,
                timestamp,
                mapping.audio_time_base,
                false,
                mapping.audio_stream_idx,
            )?),
            FlvTag::Video {
                timestamp,
                payload,
                is_keyframe,
            } => Ok(Self::make_packet(
                payload,
                timestamp,
                mapping.video_time_base,
                is_keyframe,
                mapping.video_stream_idx,
            )?),
            FlvTag::ScriptData(_) => {
                anyhow::bail!("ScriptData tags cannot be converted to AVPackets")
            }
        }
    }

    fn make_packet(
        payload: Bytes,
        timestamp: u32,
        time_base: AVRational,
        is_keyframe: bool,
        stream_idx: usize,
    ) -> Result<Packet> {
        let pkt = Packet::alloc()?;

        let len = payload.len();
        unsafe {
            let pkt = &mut *pkt.packet;

            let buf = av_malloc(len) as *mut u8;
            if buf.is_null() {
                anyhow::bail!("Failed to allocate memory for packet data")
            }
            buf.copy_from_nonoverlapping(payload.as_ptr(), len);

            // FLV timestamps are in milliseconds, convert to stream time base
            // AVRational { num: 1, den: 1000 } represents milliseconds
            let pts = av_rescale_q(
                timestamp as i64,
                AVRational { num: 1, den: 1000 },
                time_base,
            );

            (*pkt).data = buf;
            (*pkt).size = len as i32;
            (*pkt).stream_index = stream_idx as i32;

            (*pkt).pts = pts;
            (*pkt).dts = pts;

            if is_keyframe {
                (*pkt).flags |= ffmpeg_sys_next::AV_PKT_FLAG_KEY;
            }
        }
        Ok(pkt)
    }
}

struct FlvStreamMapping {
    audio_stream_idx: usize,
    video_stream_idx: usize,
    audio_time_base: AVRational,
    video_time_base: AVRational,
}

impl FlvStreamMapping {
    fn from_streams(streams: &dyn StreamCollection) -> Result<Self> {
        let mut audio_stream_idx: Option<usize> = None;
        let mut video_stream_idx: Option<usize> = None;
        let mut audio_time_base: Option<AVRational> = None;
        let mut video_time_base: Option<AVRational> = None;

        for i in 0..streams.stream_count() {
            let Some(stream) = streams.stream(i) else {
                continue;
            };

            let codec_params = stream.codec_params_ptr();
            match codec_params.codec_type() {
                AVMediaType::AVMEDIA_TYPE_AUDIO if audio_stream_idx.is_none() => {
                    audio_stream_idx = Some(stream.index());
                    audio_time_base = Some(stream.time_base());
                }
                AVMediaType::AVMEDIA_TYPE_VIDEO if video_stream_idx.is_none() => {
                    video_stream_idx = Some(stream.index());
                    video_time_base = Some(stream.time_base());
                }
                _ => {}
            }
        }

        let audio_stream_idx =
            audio_stream_idx.ok_or_else(|| anyhow::anyhow!("Audio stream index not found"))?;
        let video_stream_idx =
            video_stream_idx.ok_or_else(|| anyhow::anyhow!("Video stream index not found"))?;
        let audio_time_base =
            audio_time_base.ok_or_else(|| anyhow::anyhow!("Audio stream time base not found"))?;
        let video_time_base =
            video_time_base.ok_or_else(|| anyhow::anyhow!("Video stream time base not found"))?;

        Ok(Self {
            audio_stream_idx,
            video_stream_idx,
            audio_time_base,
            video_time_base,
        })
    }
}

impl TryFrom<Bytes> for FlvTag {
    type Error = anyhow::Error;

    fn try_from(data: Bytes) -> Result<Self, Self::Error> {
        // Expect one complete FLV tag block: header(11) + payload(data_size) + previous_tag_size(4)
        if data.len() < 15 {
            anyhow::bail!("FLV tag buffer too small: {}", data.len());
        }

        let tag_type = data[0];
        let data_size = ((data[1] as usize) << 16) | ((data[2] as usize) << 8) | (data[3] as usize);
        let expected_len = 11 + data_size + 4;
        if data.len() < expected_len {
            anyhow::bail!(
                "Incomplete FLV tag buffer: have {}, expect {}",
                data.len(),
                expected_len
            );
        }

        let ts_low = ((data[4] as u32) << 16) | ((data[5] as u32) << 8) | (data[6] as u32);
        let ts_ext = data[7] as u32;
        let timestamp = (ts_ext << 24) | ts_low;

        let payload = data.slice(11..(11 + data_size));

        match tag_type {
            8 => Ok(FlvTag::audio(timestamp, payload)),
            9 => Ok(FlvTag::video(timestamp, payload)),
            18 => {
                let metadata = parse_script_data_metadata(payload)?;
                Ok(FlvTag::script_data(metadata))
            }
            _ => anyhow::bail!("Unsupported FLV tag type: {}", tag_type),
        }
    }
}

fn parse_script_data_metadata(payload: Bytes) -> Result<StreamMetadata> {
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
