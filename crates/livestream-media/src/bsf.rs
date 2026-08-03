//! FFmpeg bitstream filter wrappers.
//!
//! `H264Mp4ToAnnexb` converts H.264 from AVCC (MP4) format to Annex B
//! (start-code delimited) format.

use anyhow::Result;
use ffmpeg_sys_next::*;

use crate::codec::copy_extradata_to_codecpar;
use crate::packet::Packet;

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};

// ── FFmpeg type definitions (compat for ffmpeg-sys-next 8.x) ──
// Note: If ffmpeg-sys-next 8.x adds AVBSFContext bindings, replace with
// the official types.

#[repr(C)]
struct AVBitStreamFilter {
    _private: [u8; 0],
}

#[repr(C)]
struct AVBSFContextCompat {
    av_class: *const AVClass,
    filter: *const AVBitStreamFilter,
    priv_data: *mut c_void,
    par_in: *mut AVCodecParameters,
    par_out: *mut AVCodecParameters,
    time_base_in: AVRational,
    time_base_out: AVRational,
}

unsafe extern "C" {
    fn av_bsf_get_by_name(name: *const c_char) -> *const AVBitStreamFilter;
    fn av_bsf_alloc(filter: *const AVBitStreamFilter, ctx: *mut *mut AVBSFContextCompat) -> c_int;
    fn av_bsf_init(ctx: *mut AVBSFContextCompat) -> c_int;
    fn av_bsf_send_packet(ctx: *mut AVBSFContextCompat, pkt: *mut AVPacket) -> c_int;
    fn av_bsf_receive_packet(ctx: *mut AVBSFContextCompat, pkt: *mut AVPacket) -> c_int;
    fn av_bsf_free(ctx: *mut *mut AVBSFContextCompat);
}

// ── RAII guard for BSF context construction ──

/// Guards a `*mut AVBSFContextCompat` during construction so that it is
/// freed on any early-return path.  On success, `into_inner()` disarms
/// the guard and transfers ownership to the caller.
struct BsfContextGuard(*mut AVBSFContextCompat);

impl BsfContextGuard {
    fn into_inner(mut self) -> *mut AVBSFContextCompat {
        let ctx = self.0;
        self.0 = std::ptr::null_mut();
        ctx
    }
}

impl Drop for BsfContextGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: The guard owns the pointer; free it on drop.
            unsafe {
                av_bsf_free(&mut self.0);
            }
        }
    }
}

/// H.264 bitstream filter: converts MP4-style (AVCC, length-prefixed) to
/// Annex B (start-code delimited) format.
pub(crate) struct H264Mp4ToAnnexb {
    ctx: *mut AVBSFContextCompat,
}

// SAFETY: AVBSFContext is not thread-safe — ownership transfer is fine,
// but shared access is not (no Sync).
unsafe impl Send for H264Mp4ToAnnexb {}

impl H264Mp4ToAnnexb {
    /// Create a new H.264 AVCC→AnnexB filter.
    ///
    /// `avcc_extradata` is the AVCDecoderConfigurationRecord bytes
    /// (extradata from the AVC sequence header).
    pub(crate) fn new(avcc_extradata: &[u8]) -> Result<Self> {
        let name = CString::new("h264_mp4toannexb")?;
        // SAFETY: av_bsf_get_by_name returns a static filter descriptor or NULL.
        let filter = unsafe { av_bsf_get_by_name(name.as_ptr()) };
        if filter.is_null() {
            anyhow::bail!("FFmpeg bsf h264_mp4toannexb not found");
        }

        let mut ctx: *mut AVBSFContextCompat = std::ptr::null_mut();
        // SAFETY: av_bsf_alloc allocates a new BSF context.
        let ret = unsafe { av_bsf_alloc(filter, &mut ctx) };
        if ret < 0 || ctx.is_null() {
            anyhow::bail!("Failed to allocate h264_mp4toannexb bsf: {}", ret);
        }

        // Guard ensures ctx is freed on any error path below.
        let guard = BsfContextGuard(ctx);

        // SAFETY: ctx was just allocated. Configure par_in with codec info.
        // extradata copy is separated from av_bsf_init so that the guard
        // catches failures from either step.
        unsafe {
            let codecpar = &mut *(*ctx).par_in;
            codecpar.codec_type = AVMediaType::AVMEDIA_TYPE_VIDEO;
            codecpar.codec_id = AVCodecID::AV_CODEC_ID_H264;
            copy_extradata_to_codecpar(codecpar, avcc_extradata)?;
        }

        // SAFETY: par_in is configured; av_bsf_init finalizes the filter.
        let init_result = unsafe { av_bsf_init(ctx) };
        if init_result < 0 {
            anyhow::bail!("Failed to initialize h264_mp4toannexb bsf: {}", init_result);
        }

        // Success — transfer ownership out of the guard.
        Ok(Self {
            ctx: guard.into_inner(),
        })
    }

    /// Filter a single packet through the BSF.
    ///
    /// Consumes the input packet and returns zero or more output packets.
    /// Call in a loop: send → receive → send → receive → ...
    pub(crate) fn filter(&mut self, packet: Packet) -> Result<Vec<Packet>> {
        // SAFETY: av_bsf_send_packet internally calls av_packet_ref;
        // it does not take ownership of the packet. The Rust Packet
        // wrapper is dropped normally at end of scope.
        let send_ret = unsafe { av_bsf_send_packet(self.ctx, packet.as_ptr() as *mut AVPacket) };
        if send_ret < 0 {
            anyhow::bail!("Failed to send packet into bsf: {}", send_ret);
        }

        let mut out = Vec::new();
        loop {
            let mut pkt = Packet::alloc()?;
            // SAFETY: pkt is a freshly allocated packet. av_bsf_receive_packet
            // fills it with filtered data.
            let recv_ret = unsafe { av_bsf_receive_packet(self.ctx, pkt.as_mut_ptr()) };
            if recv_ret == AVERROR_EOF || recv_ret == AVERROR(EAGAIN) {
                break;
            }
            if recv_ret < 0 {
                anyhow::bail!("Failed to receive packet from bsf: {}", recv_ret);
            }
            out.push(pkt);
        }

        Ok(out)
    }
}

impl Drop for H264Mp4ToAnnexb {
    fn drop(&mut self) {
        // SAFETY: av_bsf_free frees the BSF context and NULLs the pointer.
        unsafe {
            av_bsf_free(&mut self.ctx);
        }
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    /// AVCDecoderConfigurationRecord：version=1, profile=0x42, SPS 67 42 00 1E, PPS 68 CE 3C 80
    const AVCC: &[u8] = &[
        0x01, 0x42, 0x00, 0x1E, 0xFF, 0xE1, 0x00, 0x04, 0x67, 0x42, 0x00, 0x1E, 0x01, 0x00, 0x04,
        0x68, 0xCE, 0x3C, 0x80,
    ];

    fn make_packet_with_data(data: &[u8]) -> Packet {
        let mut pkt = Packet::alloc().unwrap();
        pkt.set_data(data).unwrap();
        pkt
    }

    #[test]
    fn new_accepts_valid_avcc() {
        assert!(H264Mp4ToAnnexb::new(AVCC).is_ok());
    }

    #[test]
    fn filter_converts_to_annex_b() {
        let mut bsf = H264Mp4ToAnnexb::new(AVCC).unwrap();
        let input = make_packet_with_data(&[0x00, 0x00, 0x00, 0x05, 0x65, 0x88, 0x84, 0x01, 0x2C]);
        let out = bsf.filter(input).unwrap();
        assert_eq!(out.len(), 1);
        let data = out[0].data();
        // Annex B 起始码，不再含长度前缀
        assert!(data.starts_with(&[0x00, 0x00, 0x00, 0x01]));
        assert!(!data.windows(4).any(|w| w == [0x00, 0x00, 0x00, 0x05]));
    }
}
