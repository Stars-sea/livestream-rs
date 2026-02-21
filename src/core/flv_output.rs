//! FLV output context for streaming to RTMP servers.

use crate::core::context::{Context, InputContext, OutputContext};

use anyhow::Result;
use ffmpeg_sys_next::*;

use std::ptr::null_mut;

/// Wrapper for FFmpeg output context configured for FLV streaming to RTMP servers.
pub struct FlvOutputContext {
    ctx: *mut AVFormatContext,
    rtmp_url: String,
}

impl FlvOutputContext {
    pub fn create(rtmp_url: String, input_ctx: &impl InputContext) -> Result<Self> {
        let ctx = Self::alloc_output_ctx("flv", &rtmp_url)?;

        if let Err(e) = Self::copy_parameters(ctx, input_ctx) {
            unsafe { avformat_free_context(ctx) };
            return Err(e);
        }

        if let Err(e) = Self::write_header(ctx) {
            unsafe { avformat_free_context(ctx) };
            return Err(e);
        }

        Ok(Self { ctx, rtmp_url })
    }

    pub fn rtmp_url(&self) -> &str {
        &self.rtmp_url
    }
}

impl Drop for FlvOutputContext {
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

impl Context for FlvOutputContext {
    fn get_ctx(&self) -> *mut AVFormatContext {
        self.ctx
    }
}

impl OutputContext for FlvOutputContext {}
