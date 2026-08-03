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

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::OwnedCodecParams;
    use crate::stream::StaticStreamCollection;
    use ffmpeg_sys_next::AVCodecID;
    use rml_rtmp::sessions::StreamMetadata;

    /// AVCDecoderConfigurationRecord：version=1, profile=0x42, SPS 67 42 00 1E, PPS 68 CE 3C 80
    const AVCC: &[u8] = &[
        0x01, 0x42, 0x00, 0x1E, 0xFF, 0xE1, 0x00, 0x04, 0x67, 0x42, 0x00, 0x1E, 0x01, 0x00, 0x04,
        0x68, 0xCE, 0x3C, 0x80,
    ];
    /// AAC-LC AudioSpecificConfig（AOT=2, 44100）
    const ASC: &[u8] = &[0x12, 0x10];
    /// 视频帧 AVCC 长度前缀 NAL（IDR）
    const NAL_FRAME: &[u8] = &[0x00, 0x00, 0x00, 0x05, 0x65, 0x88, 0x84, 0x01, 0x2C];

    fn make_streams() -> StaticStreamCollection {
        let video =
            OwnedCodecParams::create_dummy_video(AVCodecID::AV_CODEC_ID_H264, 640, 360, 30.0)
                .unwrap();
        let audio =
            OwnedCodecParams::create_dummy_audio(AVCodecID::AV_CODEC_ID_AAC, 44100, 2).unwrap();
        StaticStreamCollection::from_owned_params(vec![
            (0, AVRational { num: 1, den: 90000 }, video),
            (1, AVRational { num: 1, den: 44100 }, audio),
        ])
    }

    fn video_tag(prefix: &[u8], body: &[u8]) -> FlvTag {
        let mut payload = prefix.to_vec();
        payload.extend_from_slice(body);
        FlvTag::video(0, Bytes::from(payload))
    }

    fn video_seq_tag() -> FlvTag {
        video_tag(&[0x17, 0x00, 0, 0, 0], AVCC)
    }

    fn video_frame_tag(prefix: u8, timestamp: u32) -> FlvTag {
        let mut payload = vec![prefix, 0x01, 0, 0, 0];
        payload.extend_from_slice(NAL_FRAME);
        FlvTag::video(timestamp, Bytes::from(payload))
    }

    fn audio_seq_tag() -> FlvTag {
        let mut payload = vec![0xAF, 0x00];
        payload.extend_from_slice(ASC);
        FlvTag::audio(0, Bytes::from(payload))
    }

    fn audio_frame_tag(prefix: u8, timestamp: u32) -> FlvTag {
        let mut payload = vec![prefix, 0x01];
        payload.extend_from_slice(&[0x21, 0x00, 0x1F, 0xFC]);
        FlvTag::audio(timestamp, Bytes::from(payload))
    }

    #[test]
    fn video_seq_header_caches_config() {
        let mut packetizer = FlvTagPacketizer::new();
        let streams = make_streams();
        let packets = packetizer.packetize(&video_seq_tag(), &streams).unwrap();
        assert!(packets.is_empty());
        assert!(packetizer.avc_config_raw.is_some());
        assert!(packetizer.h264_bsf.is_some());
    }

    #[test]
    fn video_frame_goes_through_bsf() {
        let mut packetizer = FlvTagPacketizer::new();
        let streams = make_streams();
        packetizer.packetize(&video_seq_tag(), &streams).unwrap();
        let packets = packetizer
            .packetize(&video_frame_tag(0x27, 0), &streams)
            .unwrap();
        assert_eq!(packets.len(), 1);
        let data = packets[0].data();
        // Annex B 起始码
        assert!(data.starts_with(&[0x00, 0x00, 0x00, 0x01]));
        // 不再含长度前缀
        assert!(!data.windows(4).any(|w| w == [0x00, 0x00, 0x00, 0x05]));
        assert_eq!(packets[0].stream_idx(), 0);
        assert!(!packets[0].is_key_frame());
    }

    #[test]
    fn video_keyframe_flag_propagates() {
        let mut packetizer = FlvTagPacketizer::new();
        let streams = make_streams();
        packetizer.packetize(&video_seq_tag(), &streams).unwrap();
        let packets = packetizer
            .packetize(&video_frame_tag(0x17, 0), &streams)
            .unwrap();
        assert_eq!(packets.len(), 1);
        assert!(packets[0].is_key_frame());
    }

    #[test]
    fn video_frame_without_seq_header_errors() {
        let mut packetizer = FlvTagPacketizer::new();
        let streams = make_streams();
        let err = packetizer
            .packetize(&video_frame_tag(0x27, 0), &streams)
            .unwrap_err();
        assert!(err.to_string().contains("AVC sequence header missing"));
    }

    #[test]
    fn video_unsupported_codec_errors() {
        let mut packetizer = FlvTagPacketizer::new();
        let streams = make_streams();
        let err = packetizer
            .packetize(
                &video_tag(&[0x1C, 0x01, 0, 0, 0], &[0x00, 0x00, 0x00, 0x01]),
                &streams,
            )
            .unwrap_err();
        assert!(err.to_string().contains("Unsupported FLV video codec id"));
    }

    #[test]
    fn video_short_payload_errors() {
        let mut packetizer = FlvTagPacketizer::new();
        let streams = make_streams();
        let err = packetizer
            .packetize(
                &FlvTag::video(0, Bytes::from_static(&[0x17, 0x00])),
                &streams,
            )
            .unwrap_err();
        assert!(err.to_string().contains("Invalid FLV video payload size"));
    }

    #[test]
    fn video_eos_returns_empty() {
        let mut packetizer = FlvTagPacketizer::new();
        let streams = make_streams();
        packetizer.packetize(&video_seq_tag(), &streams).unwrap();
        let packets = packetizer
            .packetize(&video_tag(&[0x27, 0x02, 0, 0, 0], &[0x00]), &streams)
            .unwrap();
        assert!(packets.is_empty());
        // 未知 AVC packet type
        let err = packetizer
            .packetize(&video_tag(&[0x27, 0x03, 0, 0, 0], &[0x00]), &streams)
            .unwrap_err();
        assert!(err.to_string().contains("Unsupported AVC packet type"));
    }

    #[test]
    fn audio_seq_header_caches_asc() {
        let mut packetizer = FlvTagPacketizer::new();
        let streams = make_streams();
        let packets = packetizer.packetize(&audio_seq_tag(), &streams).unwrap();
        assert!(packets.is_empty());
        assert_eq!(packetizer.aac_asc_raw.as_deref(), Some(ASC));
    }

    #[test]
    fn audio_frame_packetized() {
        let mut packetizer = FlvTagPacketizer::new();
        let streams = make_streams();
        packetizer.packetize(&audio_seq_tag(), &streams).unwrap();
        let packets = packetizer
            .packetize(&audio_frame_tag(0xAF, 0), &streams)
            .unwrap();
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].stream_idx(), 1);
        assert!(!packets[0].is_key_frame());
    }

    #[test]
    fn audio_frame_without_seq_header_errors() {
        let mut packetizer = FlvTagPacketizer::new();
        let streams = make_streams();
        let err = packetizer
            .packetize(&audio_frame_tag(0xAF, 0), &streams)
            .unwrap_err();
        assert!(err.to_string().contains("AAC sequence header missing"));
    }

    #[test]
    fn audio_invalid_asc_errors() {
        let mut packetizer = FlvTagPacketizer::new();
        let streams = make_streams();
        // AOT=0
        let err = packetizer
            .packetize(
                &FlvTag::audio(0, Bytes::from_static(&[0xAF, 0x00, 0x00, 0x10])),
                &streams,
            )
            .unwrap_err();
        assert!(err.to_string().contains("Invalid AAC audio object type"));
        // 过短
        let err = packetizer
            .packetize(
                &FlvTag::audio(0, Bytes::from_static(&[0xAF, 0x00, 0x12])),
                &streams,
            )
            .unwrap_err();
        assert!(err.to_string().contains("too short"));
    }

    #[test]
    fn audio_unsupported_packet_type_errors() {
        let mut packetizer = FlvTagPacketizer::new();
        let streams = make_streams();
        packetizer.packetize(&audio_seq_tag(), &streams).unwrap();
        let err = packetizer
            .packetize(
                &FlvTag::audio(0, Bytes::from_static(&[0xAF, 0x02, 0x21])),
                &streams,
            )
            .unwrap_err();
        assert!(err.to_string().contains("Unsupported AAC packet type"));
    }

    #[test]
    fn mp3_audio_passes_through() {
        let mut packetizer = FlvTagPacketizer::new();
        let streams = make_streams();
        let packets = packetizer
            .packetize(
                &FlvTag::audio(0, Bytes::from_static(&[0x20, 0xFF, 0xFB, 0x90, 0x64])),
                &streams,
            )
            .unwrap();
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].stream_idx(), 1);
        assert_eq!(packets[0].data(), &[0xFF, 0xFB, 0x90, 0x64]);
    }

    #[test]
    fn pts_rescaled_to_stream_timebase() {
        let mut packetizer = FlvTagPacketizer::new();
        let streams = make_streams();
        packetizer.packetize(&video_seq_tag(), &streams).unwrap();
        packetizer.packetize(&audio_seq_tag(), &streams).unwrap();
        // 1000ms 视频帧 → 1/90000 timebase
        let packets = packetizer
            .packetize(&video_frame_tag(0x27, 1000), &streams)
            .unwrap();
        assert_eq!(packets[0].pts(), Some(90000));
        // 1000ms 音频帧 → 1/44100 timebase
        let packets = packetizer
            .packetize(&audio_frame_tag(0xAF, 1000), &streams)
            .unwrap();
        assert_eq!(packets[0].pts(), Some(44100));
    }

    #[test]
    fn script_data_returns_empty() {
        let mut packetizer = FlvTagPacketizer::new();
        let streams = make_streams();
        let packets = packetizer
            .packetize(&FlvTag::ScriptData(StreamMetadata::new()), &streams)
            .unwrap();
        assert!(packets.is_empty());
    }

    #[test]
    fn missing_video_stream_errors() {
        let audio =
            OwnedCodecParams::create_dummy_audio(AVCodecID::AV_CODEC_ID_AAC, 44100, 2).unwrap();
        let streams = StaticStreamCollection::from_owned_params(vec![(
            1,
            AVRational { num: 1, den: 44100 },
            audio,
        )]);
        let mut packetizer = FlvTagPacketizer::new();
        packetizer.packetize(&video_seq_tag(), &streams).unwrap();
        let err = packetizer
            .packetize(&video_frame_tag(0x27, 0), &streams)
            .unwrap_err();
        assert!(err.to_string().contains("Video stream not found"));
    }

    #[test]
    fn missing_audio_stream_errors() {
        let video =
            OwnedCodecParams::create_dummy_video(AVCodecID::AV_CODEC_ID_H264, 640, 360, 30.0)
                .unwrap();
        let streams = StaticStreamCollection::from_owned_params(vec![(
            0,
            AVRational { num: 1, den: 90000 },
            video,
        )]);
        let mut packetizer = FlvTagPacketizer::new();
        let err = packetizer
            .packetize(
                &FlvTag::audio(0, Bytes::from_static(&[0x20, 0xFF, 0xFB, 0x90, 0x64])),
                &streams,
            )
            .unwrap_err();
        assert!(err.to_string().contains("Audio stream not found"));
    }
}
