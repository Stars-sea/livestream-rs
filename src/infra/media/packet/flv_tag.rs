use anyhow::Result;
use bytes::Bytes;
use ffmpeg_sys_next::{AVRational, av_malloc, av_rescale_q};
use std::sync::Arc;

use super::Packet;
use crate::infra::media::{Metadata, StreamTrait, stream::DummyStream};

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
    ScriptData(Arc<dyn Metadata>),
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

    pub fn script_data(metadata: Arc<dyn Metadata>) -> Self {
        FlvTag::ScriptData(metadata)
    }

    pub fn to_packet(self, stream: &impl StreamTrait) -> Result<Option<Packet>> {
        self.convert_packet(stream.time_base(), stream.index())
    }

    pub fn convert_packet(
        self,
        time_base: AVRational,
        stream_idx: usize,
    ) -> Result<Option<Packet>> {
        let packet = match self {
            FlvTag::Audio { timestamp, payload } => Some(Self::make_packet(
                payload.clone(),
                timestamp,
                time_base,
                false,
                stream_idx,
            )?),
            FlvTag::Video {
                timestamp,
                payload,
                is_keyframe,
            } => Some(Self::make_packet(
                payload.clone(),
                timestamp,
                time_base,
                is_keyframe,
                stream_idx,
            )?),
            FlvTag::ScriptData(_) => None,
        };
        Ok(packet)
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

impl Into<Option<Packet>> for FlvTag {
    fn into(self) -> Option<Packet> {
        match self {
            FlvTag::Audio { .. } => self.to_packet(&DummyStream::Audio).ok()?,
            FlvTag::Video { .. } => self.to_packet(&DummyStream::Video).ok()?,
            FlvTag::ScriptData(_) => None,
        }
    }
}

fn is_video_keyframe(payload: &Bytes) -> bool {
    if let Some(&first_byte) = payload.first() {
        let frame_type = (first_byte & 0xF0) >> 4;
        return frame_type == 1;
    }
    false
}
