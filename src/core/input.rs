//! SRT input context wrapper for FFmpeg.

use super::context::{Context, InputContext, ffmpeg_error};

use anyhow::{Result, anyhow};
use ffmpeg_sys_next::*;

use std::ffi::{c_int, c_void};
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::debug;

/// Wrapper for FFmpeg input context configured for SRT streams.
#[derive(Debug)]
pub struct SrtInputContext {
    ctx: *mut AVFormatContext,
}

impl SrtInputContext {
    fn alloc_context(stop_signal: Arc<AtomicBool>) -> Result<*mut AVFormatContext> {
        let ctx: *mut AVFormatContext = unsafe { avformat_alloc_context() };
        if ctx.is_null() {
            return Err(anyhow!("Failed to alloc AVFormatContext"));
        }

        unsafe {
            let stop_signal_ptr = Arc::into_raw(stop_signal) as *mut c_void;
            (*ctx).interrupt_callback = AVIOInterruptCB {
                callback: Some(interrupt_callback),
                opaque: stop_signal_ptr,
            };
        }

        Ok(ctx)
    }

    /// Opens an SRT stream for reading.
    ///
    /// # Arguments
    /// * `path` - SRT URL (e.g., "srt://0.0.0.0:4000?mode=listener&passphrase=secret")
    ///
    /// # Errors
    /// Returns an error if the stream cannot be opened or stream info cannot be found.
    pub fn open(path: &str, stop_signal: Arc<AtomicBool>) -> Result<Self> {
        debug!(url = %path, "Attempting to open SRT input");
        let mut ctx = Self::alloc_context(stop_signal)?;

        let c_url = std::ffi::CString::new(path)?;
        let ret = unsafe { avformat_open_input(&mut ctx, c_url.as_ptr(), null_mut(), null_mut()) };
        if ret < 0 {
            let err_msg = ffmpeg_error(ret);
            unsafe { avformat_close_input(&mut ctx) };
            anyhow::bail!("Failed to open input: {}", err_msg);
        }

        let ret = unsafe { avformat_find_stream_info(ctx, null_mut()) };
        if ret < 0 {
            let err_msg = ffmpeg_error(ret);
            unsafe { avformat_close_input(&mut ctx) };
            anyhow::bail!("Failed to find stream info: {}", err_msg);
        }

        debug!(
            url = %path,
            "Successfully opened SRT input and found stream info"
        );
        Ok(Self { ctx })
    }
}

impl Drop for SrtInputContext {
    fn drop(&mut self) {
        if self.ctx.is_null() {
            return;
        }
        unsafe {
            let _ = Arc::from_raw((*self.ctx).interrupt_callback.opaque as *const AtomicBool);
            avio_context_free(&mut (*self.ctx).pb);
            avformat_close_input(&mut self.ctx)
        };
        self.ctx = null_mut();
    }
}

impl Context for SrtInputContext {
    fn get_ctx(&self) -> *mut AVFormatContext {
        self.ctx
    }
}

impl InputContext for SrtInputContext {}

extern "C" fn interrupt_callback(opaque: *mut c_void) -> c_int {
    if opaque.is_null() {
        return 0;
    }

    let stop_flag = unsafe { &*(opaque as *const AtomicBool) };

    if stop_flag.load(Ordering::Relaxed) {
        1
    } else {
        0
    }
}
