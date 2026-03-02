//! SRT input context wrapper for FFmpeg.

use super::context::{Context, InputContext};
use super::ffmpeg_error;
use super::options::{SrtInputStreamOptions, StreamOptions};

use anyhow::Result;
use ffmpeg_sys_next::*;

use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tracing::debug;

/// Wrapper for FFmpeg input context configured for SRT streams.
#[derive(Debug)]
pub struct SrtInputContext {
    ctx: *mut AVFormatContext,
}

impl SrtInputContext {}

impl Drop for SrtInputContext {
    fn drop(&mut self) {
        Self::free_context(self.ctx);
        self.ctx = null_mut();
    }
}

impl Context for SrtInputContext {
    fn get_ctx(&self) -> *mut AVFormatContext {
        self.ctx
    }
}

impl InputContext for SrtInputContext {
    type Options = SrtInputStreamOptions;

    fn open(options: &Self::Options, stop_signal: Arc<AtomicBool>) -> Result<Self> {
        debug!(url = %options.filename(), "Attempting to open SRT input");
        let mut ctx = <Self as InputContext>::alloc_context(stop_signal)?;

        let c_url = std::ffi::CString::new(options.filename())?;
        let mut c_opt = options.to_dict();

        let ret = unsafe { avformat_open_input(&mut ctx, c_url.as_ptr(), null_mut(), &mut c_opt) };
        if !c_opt.is_null() {
            unsafe { av_dict_free(&mut c_opt) };
        }

        if ret < 0 {
            let err_msg = ffmpeg_error(ret);
            Self::free_context(ctx);
            anyhow::bail!("Failed to open input: {}", err_msg);
        }

        let ret = unsafe { avformat_find_stream_info(ctx, null_mut()) };
        if ret < 0 {
            let err_msg = ffmpeg_error(ret);
            Self::free_context(ctx);
            anyhow::bail!("Failed to find stream info: {}", err_msg);
        }

        debug!(
            url = %options.filename(),
            "Successfully opened SRT input and found stream info"
        );
        Ok(Self { ctx })
    }
}
