//! FLV tag → FFmpeg Packet converter.
//!
//! Depends on FFmpeg (Packet, StreamCollection, BSF) — lives in `livestream-media`.

use anyhow::Result;
use bytes::Bytes;
use ffmpeg_sys_next::*;

use crate::bsf::H264Mp4ToAnnexb;
use crate::context::Context;
use crate::packet::Packet;
use crate::stream::StreamCollection;

use super::mapping::FlvStreamMapping;
use super::tag::FlvTag;

/// Converts `FlvTag` → `Vec<Packet>` for FFmpeg muxer consumption.
///
/// Caches AVC config and AAC AudioSpecificConfig so they can be applied
/// to the output context's extradata via `apply_codec_extradata()`.
#[derive(Default)]
pub struct FlvTagPacketizer {
    avc_config_raw: Option<Vec<u8>>,
    aac_asc_raw: Option<Vec<u8>>,
    h264_bsf: Option<H264Mp4ToAnnexb>,
    /// FLV → stream index/timebase mapping. Invariant for the context
    /// lifetime, so it is built once (on the first `packetize` call) and
    /// reused for every tag instead of being rebuilt per packet.
    mapping: Option<FlvStreamMapping>,
}

impl FlvTagPacketizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Convert a single FLV tag into zero or more FFmpeg `Packet`s.
    ///
    /// Sequence headers are cached (returning zero packets).
    /// Video frames pass through the H.264 Annex B bitstream filter.
    pub fn packetize(
        &mut self,
        tag: &FlvTag,
        streams: &dyn StreamCollection,
    ) -> Result<Vec<Packet>> {
        // The mapping is invariant for this context's lifetime — compute it
        // at most once and cache it (streams must not change between calls).
        // Take it out so the match below can borrow `self` mutably, then
        // restore it for the next call.
        let mapping = self
            .mapping
            .take()
            .unwrap_or_else(|| FlvStreamMapping::from_streams(streams));

        let packets = match tag {
            FlvTag::Audio { timestamp, payload } => {
                self.packetize_audio(*timestamp, payload, &mapping)
            }
            FlvTag::Video {
                timestamp,
                payload,
                is_keyframe,
            } => self.packetize_video(*timestamp, payload, *is_keyframe, &mapping),
            FlvTag::ScriptData(_) => Ok(Vec::new()),
        };

        // Restore the cached mapping for subsequent calls.
        self.mapping = Some(mapping);
        packets
    }

    /// Apply cached codec extradata to an output context.
    ///
    /// Must be called before writing packets to the muxer.
    pub fn apply_codec_extradata(&self, output_ctx: &impl Context) -> Result<()> {
        if let Some(avcc) = &self.avc_config_raw {
            set_stream_extradata(output_ctx, AVMediaType::AVMEDIA_TYPE_VIDEO, avcc)?;
        }
        if let Some(asc) = &self.aac_asc_raw {
            set_stream_extradata(output_ctx, AVMediaType::AVMEDIA_TYPE_AUDIO, asc)?;
        }
        Ok(())
    }

    // ── Private helpers ──

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
                // AVC sequence header — cache and create BSF.
                self.avc_config_raw = Some(avc_payload.to_vec());
                self.h264_bsf = Some(H264Mp4ToAnnexb::new(avc_payload)?);
                Ok(Vec::new())
            }
            1 => {
                let (video_stream_idx, video_time_base) = get_video_stream(mapping)?;
                let packet = make_packet(
                    avc_payload,
                    timestamp,
                    video_time_base,
                    is_keyframe,
                    video_stream_idx,
                )?;

                let bsf = self.h264_bsf.as_mut().ok_or(anyhow::anyhow!(
                    "AVC sequence header missing before video frame"
                ))?;
                bsf.filter(packet)
            }
            2 => Ok(Vec::new()), // AVC end-of-sequence
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
            return self.packetize_aac(timestamp, payload[1], &payload[2..], mapping);
        }

        // Non-AAC audio (MP3, PCM, etc.)
        let (audio_stream_idx, audio_time_base) = get_audio_stream(mapping)?;
        let raw_audio = &payload[1..];
        let packet = make_packet(
            raw_audio,
            timestamp,
            audio_time_base,
            false,
            audio_stream_idx,
        )?;
        Ok(vec![packet])
    }

    fn packetize_aac(
        &mut self,
        timestamp: u32,
        aac_packet_type: u8,
        aac_payload: &[u8],
        mapping: &FlvStreamMapping,
    ) -> Result<Vec<Packet>> {
        match aac_packet_type {
            0 => {
                validate_aac_audio_config(aac_payload)?;
                self.aac_asc_raw = Some(aac_payload.to_vec());
                Ok(Vec::new())
            }
            1 if self.aac_asc_raw.is_none() => {
                anyhow::bail!("AAC sequence header missing before raw frame");
            }
            1 => {
                let (audio_stream_idx, audio_time_base) = get_audio_stream(mapping)?;
                let packet = make_packet(
                    aac_payload,
                    timestamp,
                    audio_time_base,
                    false,
                    audio_stream_idx,
                )?;
                Ok(vec![packet])
            }
            x => anyhow::bail!("Unsupported AAC packet type: {}", x),
        }
    }
}

// ── Internal helpers ──

/// Construct an `AVPacket` from raw media data.
fn make_packet(
    payload: &[u8],
    timestamp: u32,
    time_base: AVRational,
    is_keyframe: bool,
    stream_idx: usize,
) -> Result<Packet> {
    let mut pkt = Packet::alloc()?;

    pkt.set_data(payload)?;

    // SAFETY: pkt is valid. Rescale timestamp from ms to target timebase.
    unsafe {
        let ptr = pkt.as_mut_ptr();
        let pts = av_rescale_q(
            timestamp as i64,
            AVRational { num: 1, den: 1000 },
            time_base,
        );
        (*ptr).stream_index = stream_idx as i32;
        (*ptr).pts = pts;
        (*ptr).dts = pts;

        if is_keyframe {
            (*ptr).flags |= AV_PKT_FLAG_KEY;
        }
    }

    Ok(pkt)
}

/// Validate AAC AudioSpecificConfig.
fn validate_aac_audio_config(data: &[u8]) -> Result<()> {
    if data.len() < 2 {
        anyhow::bail!("AAC AudioSpecificConfig too short: {}", data.len());
    }
    let audio_object_type = (data[0] >> 3) & 0x1F;
    if audio_object_type == 0 {
        anyhow::bail!("Invalid AAC audio object type: 0");
    }
    Ok(())
}

/// Set stream extradata on an output context for the given media type.
fn set_stream_extradata(ctx: &impl Context, media_type: AVMediaType, data: &[u8]) -> Result<()> {
    if !ctx.available() {
        anyhow::bail!("Output format context is null");
    }

    // SAFETY: Find the matching stream index.
    unsafe {
        let format_ctx = ctx.ptr();
        let nb_streams = (*format_ctx).nb_streams as usize;

        let stream_idx = (0..nb_streams)
            .find(|&i| {
                let stream = *(*format_ctx).streams.add(i);
                !stream.is_null()
                    && !(*stream).codecpar.is_null()
                    && (*(*stream).codecpar).codec_type == media_type
            })
            .ok_or_else(|| {
                anyhow::anyhow!("Target stream for media type {:?} not found", media_type)
            })?;

        let stream = *(*format_ctx).streams.add(stream_idx);
        let codecpar = &mut *(*stream).codecpar;
        crate::codec::copy_extradata_to_codecpar(codecpar, data)?;
    }

    Ok(())
}

fn get_audio_stream(mapping: &FlvStreamMapping) -> Result<(usize, ffmpeg_sys_next::AVRational)> {
    mapping
        .audio
        .ok_or_else(|| anyhow::anyhow!("Audio stream not found in stream collection"))
}

fn get_video_stream(mapping: &FlvStreamMapping) -> Result<(usize, ffmpeg_sys_next::AVRational)> {
    mapping
        .video
        .ok_or_else(|| anyhow::anyhow!("Video stream not found in stream collection"))
}
