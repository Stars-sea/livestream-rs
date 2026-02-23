use bytes::{Buf, BytesMut};

#[derive(Clone, Debug)]
pub enum FlvTag {
    Audio {
        timestamp: u32,
        payload: Vec<u8>,
    },
    Video {
        timestamp: u32,
        payload: Vec<u8>,
        is_keyframe: bool,
    },
    ScriptData {
        timestamp: u32,
        payload: Vec<u8>,
    },
}

pub struct FlvDemuxer {
    buffer: BytesMut,
    header_parsed: bool,
}

impl FlvDemuxer {
    pub fn new() -> Self {
        Self {
            buffer: BytesMut::new(),
            header_parsed: false,
        }
    }

    pub fn push_data(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    pub fn next_tag(&mut self) -> Option<FlvTag> {
        loop {
            // SKIP FLV Header (9 bytes) + PreviousTagSize0 (4 bytes)
            if !self.header_parsed {
                if self.buffer.len() < 13 {
                    return None;
                }
                self.buffer.advance(13);
                self.header_parsed = true;
            }

            // Ananlyze the FLV tag header(11 bytes)
            if self.buffer.len() < 11 {
                return None;
            }

            let tag_type = self.buffer[0];
            let data_size = ((self.buffer[1] as usize) << 16)
                | ((self.buffer[2] as usize) << 8)
                | (self.buffer[3] as usize);

            let ts_low = ((self.buffer[4] as u32) << 16)
                | ((self.buffer[5] as u32) << 8)
                | (self.buffer[6] as u32);
            let ts_ext = self.buffer[7] as u32;
            let timestamp = (ts_ext << 24) | ts_low;

            // A total tag consists of: Header(11) + Payload(data_size) + PreviousTagSize(4)
            let total_tag_len = 11 + data_size + 4;
            if self.buffer.len() < total_tag_len {
                return None; // Not enough data for a complete tag, wait for more data
            }

            // Get the payload data
            let payload = self.buffer[11..11 + data_size].to_vec();

            // Advance the buffer to the next tag (skip current tag and its previous tag size)
            self.buffer.advance(total_tag_len);

            match tag_type {
                8 => return Some(FlvTag::Audio { timestamp, payload }),
                9 => {
                    let is_keyframe = if payload.len() > 0 {
                        (payload[0] >> 4) == 1 || (payload[0] >> 4) == 4
                    } else {
                        false
                    };
                    return Some(FlvTag::Video {
                        timestamp,
                        payload,
                        is_keyframe,
                    });
                }
                18 => return Some(FlvTag::ScriptData { timestamp, payload }),
                _ => continue, // Unknown tag type, loop again to find next tag
            }
        }
    }
}
