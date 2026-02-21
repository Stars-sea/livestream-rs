//! FLV output context for streaming to RTMP servers.

use crate::core::context::{Context, InputContext, OutputContext, ffmpeg_error};

use anyhow::Result;
use ffmpeg_sys_next::*;
use log::warn;

use std::ptr::null_mut;

/// Wrapper for FFmpeg output context configured for FLV streaming to RTMP servers.
pub struct RtmpOutputContext {
    ctx: *mut AVFormatContext,
    rtmp_url: String,
}

impl RtmpOutputContext {
    pub fn create(rtmp_url: String, input_ctx: &impl InputContext) -> Result<Self> {
        let ctx = Self::alloc_output_ctx("flv", &rtmp_url)?;

        if let Err(e) = Self::copy_streams(ctx, input_ctx) {
            warn!("Failed to copy streams to FLV output context: {e}");
            unsafe { avformat_free_context(ctx) };
            return Err(e);
        }

        if unsafe { (*ctx).pb.is_null() } {
            match Self::open_io(rtmp_url.clone(), AVIO_FLAG_WRITE) {
                Ok(pb) => unsafe { (*ctx).pb = pb },
                Err(e) => {
                    warn!("Failed to open RTMP IO context: {e}");
                    unsafe { avformat_free_context(ctx) };
                    return Err(e);
                }
            }
        }

        if let Err(e) = Self::write_header(ctx) {
            warn!("Failed to write header for FLV output context: {e}");
            unsafe { avformat_free_context(ctx) };
            return Err(e);
        }

        Ok(Self { ctx, rtmp_url })
    }

    pub fn rtmp_url(&self) -> &str {
        &self.rtmp_url
    }
}

impl Drop for RtmpOutputContext {
    fn drop(&mut self) {
        if self.ctx.is_null() {
            return;
        }

        self.write_trailer().ok();
        unsafe {
            avformat_free_context(self.ctx);
        }
        self.ctx = null_mut();
    }
}

impl Context for RtmpOutputContext {
    fn get_ctx(&self) -> *mut AVFormatContext {
        self.ctx
    }
}

impl OutputContext for RtmpOutputContext {
    fn copy_streams(ctx_ptr: *mut AVFormatContext, input_ctx: &impl InputContext) -> Result<()> {
        for i in 0..input_ctx.nb_streams() {
            let in_stream = input_ctx.stream(i).unwrap();
            let out_stream = unsafe { avformat_new_stream(ctx_ptr, null_mut()) };
            if out_stream.is_null() {
                anyhow::bail!("Failed to allocate output stream");
            }

            let ret = unsafe {
                avcodec_parameters_copy((*out_stream).codecpar, in_stream.codec_params())
            };

            if ret < 0 {
                anyhow::bail!("Failed to copy streams parameters: {}", ffmpeg_error(ret));
            }

            unsafe {
                (*(*out_stream).codecpar).codec_tag = 0;
                (*out_stream).time_base = in_stream.time_base();
            }
        }

        Ok(())
    }
}
