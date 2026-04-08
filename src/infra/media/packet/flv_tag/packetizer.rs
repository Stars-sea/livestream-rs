use anyhow::Result;
use bytes::Bytes;
use ffmpeg_sys_next::AVMediaType;

use crate::infra::media::bsf::H264Mp4ToAnnexb;
use crate::infra::media::context::Context;
use crate::infra::media::packet::Packet;
use crate::infra::media::stream::StreamCollection;
use crate::infra::media::utils::{make_packet, set_stream_extradata, validate_aac_audio_config};

use super::mapping::FlvStreamMapping;
use super::tag::FlvTag;

#[derive(Default)]
pub struct FlvTagPacketizer {
    avc_config_raw: Option<Vec<u8>>,
    aac_asc_raw: Option<Vec<u8>>,
    h264_bsf: Option<H264Mp4ToAnnexb>,
}

impl FlvTagPacketizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn packetize(
        &mut self,
        tag: &FlvTag,
        streams: &dyn StreamCollection,
    ) -> Result<Vec<Packet>> {
        let mapping = FlvStreamMapping::from_streams(streams)?;

        match tag {
            FlvTag::Audio { timestamp, payload } => {
                self.packetize_audio(*timestamp, payload, &mapping)
            }
            FlvTag::Video {
                timestamp,
                payload,
                is_keyframe,
            } => self.packetize_video(*timestamp, payload, *is_keyframe, &mapping),
            FlvTag::ScriptData(_) => Ok(Vec::new()),
        }
    }

    pub fn apply_codec_extradata(&self, output_ctx: &impl Context) -> Result<()> {
        if let Some(avcc) = &self.avc_config_raw {
            set_stream_extradata(output_ctx, AVMediaType::AVMEDIA_TYPE_VIDEO, avcc)?;
        }
        if let Some(asc) = &self.aac_asc_raw {
            set_stream_extradata(output_ctx, AVMediaType::AVMEDIA_TYPE_AUDIO, asc)?;
        }
        Ok(())
    }

    fn packetize_video(
        &mut self,
        timestamp: u32,
        payload: &Bytes,
        is_keyframe: bool,
        mapping: &FlvStreamMapping,
    ) -> Result<Vec<Packet>> {
        if payload.len() < 5 {
            anyhow::bail!("Invalid FLV video payload size: {}", payload.len());
        }

        let codec_id = payload[0] & 0x0F;
        if codec_id != 7 {
            anyhow::bail!("Unsupported FLV video codec id: {}", codec_id);
        }

        let avc_packet_type = payload[1];
        let avc_payload = &payload[5..];

        match avc_packet_type {
            0 => {
                self.avc_config_raw = Some(avc_payload.to_vec());
                self.h264_bsf = Some(H264Mp4ToAnnexb::new(avc_payload)?);
                Ok(Vec::new())
            }
            1 => {
                let packet = make_packet(
                    avc_payload,
                    timestamp,
                    mapping.video_time_base,
                    is_keyframe,
                    mapping.video_stream_idx,
                )?;

                let bsf = self.h264_bsf.as_mut().ok_or_else(|| {
                    anyhow::anyhow!("AVC sequence header missing before video frame")
                })?;
                bsf.filter(packet)
            }
            2 => Ok(Vec::new()),
            x => anyhow::bail!("Unsupported AVC packet type: {}", x),
        }
    }

    fn packetize_audio(
        &mut self,
        timestamp: u32,
        payload: &Bytes,
        mapping: &FlvStreamMapping,
    ) -> Result<Vec<Packet>> {
        if payload.len() < 2 {
            anyhow::bail!("Invalid FLV audio payload size: {}", payload.len());
        }

        let sound_format = payload[0] >> 4;
        if sound_format == 10 {
            let aac_packet_type = payload[1];
            let aac_payload = &payload[2..];

            match aac_packet_type {
                0 => {
                    validate_aac_audio_config(aac_payload)?;
                    self.aac_asc_raw = Some(aac_payload.to_vec());
                    Ok(Vec::new())
                }
                1 => {
                    if self.aac_asc_raw.is_none() {
                        anyhow::bail!("AAC sequence header missing before raw frame");
                    }

                    let packet = make_packet(
                        aac_payload,
                        timestamp,
                        mapping.audio_time_base,
                        false,
                        mapping.audio_stream_idx,
                    )?;
                    Ok(vec![packet])
                }
                x => anyhow::bail!("Unsupported AAC packet type: {}", x),
            }
        } else {
            let raw_audio = &payload[1..];
            let packet = make_packet(
                raw_audio,
                timestamp,
                mapping.audio_time_base,
                false,
                mapping.audio_stream_idx,
            )?;
            Ok(vec![packet])
        }
    }
}
