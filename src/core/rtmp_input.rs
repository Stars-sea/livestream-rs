//! RTMP input context wrapper for FFmpeg.

use super::context::{Context, InputContext};
use super::ffmpeg_error;
use super::options::{RtmpInputStreamOptions, StreamOptions};

use anyhow::Result;
use ffmpeg_sys_next::*;

use std::ffi::CString;
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tracing::debug;

/// Wrapper for FFmpeg input context configured for RTMP streams.
#[derive(Debug)]
pub struct RtmpInputContext {
    ctx: *mut AVFormatContext,
}

impl RtmpInputContext {}

impl Drop for RtmpInputContext {
    fn drop(&mut self) {
        Self::free_context(self.ctx);
        self.ctx = null_mut();
    }
}

impl Context for RtmpInputContext {
    fn get_ctx(&self) -> *mut AVFormatContext {
        self.ctx
    }
}

impl InputContext for RtmpInputContext {
    type Options = RtmpInputStreamOptions;

    fn open(options: &Self::Options, stop_signal: Arc<AtomicBool>) -> Result<Self> {
        debug!(url = %options.filename(), "Attempting to open RTMP input");

        let mut ctx = <Self as InputContext>::alloc_context(stop_signal)?;
        let c_url = CString::new(options.filename())?;
        let mut c_opt = options.to_dict();

        let ret = unsafe { avformat_open_input(&mut ctx, c_url.as_ptr(), null_mut(), &mut c_opt) };

        if !c_opt.is_null() {
            unsafe { av_dict_free(&mut c_opt) };
        }

        if ret < 0 {
            let err_msg = ffmpeg_error(ret);
            Self::free_context(ctx);
            anyhow::bail!("Failed to open RTMP input: {}", err_msg);
        }

        let ret = unsafe { avformat_find_stream_info(ctx, null_mut()) };
        if ret < 0 {
            let err_msg = ffmpeg_error(ret);
            Self::free_context(ctx);
            anyhow::bail!("Failed to find stream info: {}", err_msg);
        }

        debug!(
            url = %options.filename(),
            "Successfully opened RTMP input and found stream info"
        );

        Ok(Self { ctx })
    }
}
