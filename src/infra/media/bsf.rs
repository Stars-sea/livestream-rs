use std::os::raw::{c_char, c_int, c_void};

use anyhow::Result;
use ffmpeg_sys_next::*;

use crate::infra::media::packet::Packet;
use crate::infra::media::utils::copy_extradata_to_codecpar;

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
    fn av_bsf_send_packet(
        ctx: *mut AVBSFContextCompat,
        pkt: *mut ffmpeg_sys_next::AVPacket,
    ) -> c_int;
    fn av_bsf_receive_packet(
        ctx: *mut AVBSFContextCompat,
        pkt: *mut ffmpeg_sys_next::AVPacket,
    ) -> c_int;
    fn av_bsf_free(ctx: *mut *mut AVBSFContextCompat);
}

pub(crate) struct H264Mp4ToAnnexb {
    ctx: *mut AVBSFContextCompat,
}

unsafe impl Send for H264Mp4ToAnnexb {}
unsafe impl Sync for H264Mp4ToAnnexb {}

impl H264Mp4ToAnnexb {
    pub(crate) fn new(avcc_extradata: &[u8]) -> Result<Self> {
        let name = std::ffi::CString::new("h264_mp4toannexb")?;
        let filter = unsafe { av_bsf_get_by_name(name.as_ptr()) };
        if filter.is_null() {
            anyhow::bail!("FFmpeg bsf h264_mp4toannexb not found");
        }

        let mut ctx: *mut AVBSFContextCompat = std::ptr::null_mut();
        let ret = unsafe { av_bsf_alloc(filter, &mut ctx) };
        if ret < 0 || ctx.is_null() {
            anyhow::bail!("Failed to allocate h264_mp4toannexb bsf: {}", ret);
        }

        let init_result = unsafe {
            let codecpar = &mut *(*ctx).par_in;
            codecpar.codec_type = AVMediaType::AVMEDIA_TYPE_VIDEO;
            codecpar.codec_id = AVCodecID::AV_CODEC_ID_H264;
            copy_extradata_to_codecpar(codecpar, avcc_extradata)?;
            av_bsf_init(ctx)
        };
        if init_result < 0 {
            unsafe { av_bsf_free(&mut ctx) };
            anyhow::bail!("Failed to initialize h264_mp4toannexb bsf: {}", init_result);
        }

        Ok(Self { ctx })
    }

    pub(crate) fn filter(&mut self, packet: Packet) -> Result<Vec<Packet>> {
        let mut packet = packet;
        let send_ret = unsafe { av_bsf_send_packet(self.ctx, packet.ptr()) };
        if send_ret < 0 {
            anyhow::bail!("Failed to send packet into bsf: {}", send_ret);
        }

        let mut out = Vec::new();
        loop {
            let mut pkt = Packet::alloc()?;
            let recv_ret = unsafe { av_bsf_receive_packet(self.ctx, pkt.ptr()) };
            if recv_ret == AVERROR(EAGAIN) || recv_ret == AVERROR_EOF {
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
        unsafe { av_bsf_free(&mut self.ctx) };
    }
}
