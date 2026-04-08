mod mapping;
mod packetizer;
mod tag;

use anyhow::Result;
use bytes::Bytes;

pub use packetizer::FlvTagPacketizer;
pub use tag::FlvTag;

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
                let metadata = tag::parse_script_data_metadata(payload)?;
                Ok(FlvTag::script_data(metadata))
            }
            _ => anyhow::bail!("Unsupported FLV tag type: {}", tag_type),
        }
    }
}
