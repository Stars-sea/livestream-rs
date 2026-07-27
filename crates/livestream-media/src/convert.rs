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
use livestream_codec::{EncodedPacket, NalData};
use livestream_core::types::Codec;

use crate::packet::Packet;

/// Convert to an `AVPacket` with timestamps in **millisecond** timebase (`{1, 1000}`).
///
/// Caller must `rescale_ts()` before writing to a muxer with a different timebase.
pub trait IntoAvPacket {
    fn to_av_packet(&self) -> Result<Packet>;
}

/// Convert from an `AVPacket`. Timestamps are rescaled from the given
/// `time_base` to milliseconds via `av_packet_rescale_ts`.
/// The packet is mutated in place (timestamps are rescaled).
pub trait FromAvPacket {
    fn from_av_packet(
        pkt: &mut Packet,
        time_base: AVRational,
        codec: Codec,
    ) -> Result<EncodedPacket>;
}

impl IntoAvPacket for EncodedPacket {
    fn to_av_packet(&self) -> Result<Packet> {
        let mut pkt = Packet::alloc()?;

        // NOTE: set_data() calls av_new_packet which resets side-data fields.
        // Must set metadata AFTER copying data.
        if !self.data.is_empty() {
            pkt.set_data(self.data.as_bytes())?;
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
    fn from_av_packet(
        pkt: &mut Packet,
        time_base: AVRational,
        codec: Codec,
    ) -> Result<EncodedPacket> {
        // Rescale timestamps from the original timebase to milliseconds.
        // av_packet_rescale_ts handles AV_NOPTS_VALUE correctly (leaves it).
        pkt.rescale_ts(time_base, AVRational { num: 1, den: 1000 });

        let pts_ms = pkt.pts();
        // Fallback: when DTS is missing, use PTS. When both are missing, use 0.
        // This prevents `AV_NOPTS_VALUE` reaching the TS muxer. Consecutive
        // zeros are non-monotonic on their own, but the HLS path has a separate
        // `last_dts` enforcement in `write_packet()`; the FLV path only uses PTS.
        let dts_ms = pkt.dts().or(pts_ms).or(Some(0));

        Ok(EncodedPacket {
            codec,
            stream_index: pkt.stream_idx(),
            data: NalData::AnnexB(bytes::Bytes::copy_from_slice(pkt.data())),
            pts_ms,
            dts_ms,
            is_keyframe: pkt.is_key_frame(),
            is_sequence_header: false,
            is_script_data: false,
            extradata: None,
        })
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_keyframe_roundtrip() {
        let data: &[u8] = &[0x00, 0x00, 0x01, 0x67, 0x42];
        let pkt = EncodedPacket::new_video_keyframe(data, 1000, 900, 0);

        let mut av_pkt = pkt.to_av_packet().unwrap();
        assert_eq!(av_pkt.pts(), Some(1000));
        assert_eq!(av_pkt.dts(), Some(900));
        assert!(av_pkt.is_key_frame());

        let roundtripped = EncodedPacket::from_av_packet(
            &mut av_pkt,
            AVRational { num: 1, den: 1000 },
            Codec::H264,
        )
        .unwrap();
        assert_eq!(roundtripped.pts_ms, Some(1000));
        assert_eq!(roundtripped.dts_ms, Some(900));
        assert!(roundtripped.is_keyframe);
        assert_eq!(roundtripped.data.as_bytes(), data);
    }

    #[test]
    fn audio_roundtrip() {
        let data: &[u8] = &[0xff, 0xf1, 0x50, 0x80];
        let pkt = EncodedPacket::new_audio(data, 500, 1);

        let mut av_pkt = pkt.to_av_packet().unwrap();
        let roundtripped = EncodedPacket::from_av_packet(
            &mut av_pkt,
            AVRational { num: 1, den: 1000 },
            Codec::Aac,
        )
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

        let mut av_pkt = pkt.to_av_packet().unwrap();
        let roundtripped = EncodedPacket::from_av_packet(
            &mut av_pkt,
            AVRational { num: 1, den: 1000 },
            Codec::H264,
        )
        .unwrap();

        // PTS was None; rescale_ts preserves AV_NOPTS_VALUE → None.
        // DTS falls back to PTS (None), then to 0.
        assert_eq!(roundtripped.pts_ms, None);
        assert_eq!(roundtripped.dts_ms, Some(0));
    }
}
