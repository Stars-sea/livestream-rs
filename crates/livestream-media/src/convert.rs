//! EncodedPacket ↔ Packet conversion bridge.
//!
//! Lives in `livestream-media` because it is the only crate with FFmpeg access.
//! Provides trait extensions (`IntoAvPacket` / `FromAvPacket`) on
//! `EncodedPacket` (defined in `livestream-codec`).
//!
//! ## Timebase Convention
//!
//! `EncodedPacket` uses **millisecond** precision (`time_base = {1, 1000}`).
//! `to_av_packet()` creates an `AVPacket` in this timebase.
//! Callers must call `pkt.rescale_ts({1, 1000}, target_tb)` before writing
//! to a muxer with a different timebase.

use anyhow::Result;
use ffmpeg_sys_next::*;
use livestream_codec::EncodedPacket;
use livestream_core::types::Codec;

use crate::packet::Packet;

/// Convert to an `AVPacket` with timestamps in **millisecond** timebase (`{1, 1000}`).
///
/// Caller must `rescale_ts()` before writing to a muxer with a different timebase.
pub trait IntoAvPacket {
    fn to_av_packet(&self) -> Result<Packet>;
}

/// Convert from an `AVPacket`. Timestamps are assumed to be in the given
/// `time_base` and are converted to milliseconds.
pub trait FromAvPacket {
    fn from_av_packet(pkt: &Packet, time_base: AVRational, codec: Codec) -> Result<EncodedPacket>;
}

impl IntoAvPacket for EncodedPacket {
    fn to_av_packet(&self) -> Result<Packet> {
        let mut pkt = Packet::alloc()?;

        // NOTE: set_data() calls av_new_packet which resets side-data fields.
        // Must set metadata AFTER copying data.
        if !self.data.is_empty() {
            pkt.set_data(&self.data)?;
        }

        // SAFETY: pkt is a valid AVPacket with data allocated.
        unsafe {
            let ptr = pkt.as_mut_ptr();
            (*ptr).pts = self.pts_ms.unwrap_or(AV_NOPTS_VALUE);
            (*ptr).dts = self.dts_ms.unwrap_or(AV_NOPTS_VALUE);
            (*ptr).stream_index = self.stream_index as i32;
            if self.is_keyframe {
                (*ptr).flags |= AV_PKT_FLAG_KEY;
            }
        }

        Ok(pkt)
    }
}

impl FromAvPacket for EncodedPacket {
    fn from_av_packet(pkt: &Packet, time_base: AVRational, codec: Codec) -> Result<EncodedPacket> {
        let ptr = pkt.as_ptr();
        Ok(EncodedPacket {
            codec,
            stream_index: unsafe { (*ptr).stream_index as usize },
            data: bytes::Bytes::copy_from_slice(pkt.data()),
            pts_ms: pts_to_ms(unsafe { (*ptr).pts }, time_base),
            dts_ms: pts_to_ms(unsafe { (*ptr).dts }, time_base),
            is_keyframe: pkt.is_key_frame(),
            is_sequence_header: false,
            is_script_data: false,
            extradata: None,
        })
    }
}

/// Convert FFmpeg timestamp (in the given timebase) to milliseconds.
fn pts_to_ms(pts: i64, time_base: AVRational) -> Option<i64> {
    if pts == AV_NOPTS_VALUE || time_base.num <= 0 || time_base.den <= 0 {
        return None;
    }
    // pts * time_base.num / time_base.den * 1000
    Some((pts as i128 * time_base.num as i128 * 1000 / time_base.den as i128) as i64)
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_keyframe_roundtrip() {
        let data: &[u8] = &[0x00, 0x00, 0x01, 0x67, 0x42];
        let pkt = EncodedPacket::new_video_keyframe(data, 1000, 900, 0);

        let av_pkt = pkt.to_av_packet().unwrap();
        assert_eq!(av_pkt.pts(), Some(1000));
        assert_eq!(av_pkt.dts(), Some(900));
        assert!(av_pkt.is_key_frame());

        let roundtripped =
            EncodedPacket::from_av_packet(&av_pkt, AVRational { num: 1, den: 1000 }, Codec::H264)
                .unwrap();
        assert_eq!(roundtripped.pts_ms, Some(1000));
        assert_eq!(roundtripped.dts_ms, Some(900));
        assert!(roundtripped.is_keyframe);
        assert_eq!(roundtripped.data, data);
    }

    #[test]
    fn audio_roundtrip() {
        let data: &[u8] = &[0xff, 0xf1, 0x50, 0x80];
        let pkt = EncodedPacket::new_audio(data, 500, 1);

        let av_pkt = pkt.to_av_packet().unwrap();
        let roundtripped =
            EncodedPacket::from_av_packet(&av_pkt, AVRational { num: 1, den: 1000 }, Codec::Aac)
                .unwrap();

        assert_eq!(roundtripped.codec, Codec::Aac);
        assert_eq!(roundtripped.pts_ms, Some(500));
        assert!(!roundtripped.is_keyframe);
    }

    #[test]
    fn av_nopts_roundtrip() {
        let data: &[u8] = &[0x00];
        let mut pkt = EncodedPacket::new_video_keyframe(data, 0, 0, 0);
        pkt.pts_ms = None;
        pkt.dts_ms = None;

        let av_pkt = pkt.to_av_packet().unwrap();
        let roundtripped =
            EncodedPacket::from_av_packet(&av_pkt, AVRational { num: 1, den: 1000 }, Codec::H264)
                .unwrap();

        assert_eq!(roundtripped.pts_ms, None);
        assert_eq!(roundtripped.dts_ms, None);
    }

    #[test]
    fn pts_to_ms_correct() {
        // 90000 ticks at {1, 90000} = 1 second = 1000 ms
        assert_eq!(
            pts_to_ms(90000, AVRational { num: 1, den: 90000 }),
            Some(1000)
        );
        // 44100 ticks at {1, 44100} = 1 second
        assert_eq!(
            pts_to_ms(44100, AVRational { num: 1, den: 44100 }),
            Some(1000)
        );
    }

    #[test]
    fn pts_to_ms_invalid_timebase() {
        assert_eq!(pts_to_ms(1000, AVRational { num: 0, den: 1000 }), None);
        assert_eq!(pts_to_ms(1000, AVRational { num: 1, den: 0 }), None);
    }

    #[test]
    fn pts_to_ms_nopts_value() {
        assert_eq!(
            pts_to_ms(AV_NOPTS_VALUE, AVRational { num: 1, den: 1000 }),
            None
        );
    }
}
