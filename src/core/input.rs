//! SRT input context wrapper for FFmpeg.

use super::context::{Context, InputContext, ffmpeg_error};

use anyhow::{Result, anyhow};
use ffmpeg_sys_next::*;

use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Wrapper for FFmpeg input context configured for SRT streams.
///
/// # Safety
/// Manages the lifecycle of AVFormatContext through RAII.
/// The context is opened in `open()` and closed in `Drop`.
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
            let callback_opaque = &*stop_signal as *const AtomicBool as *mut c_void;
            (*ctx).interrupt_callback = AVIOInterruptCB {
                callback: Some(interrupt_callback),
                opaque: callback_opaque,
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
        let mut ctx = Self::alloc_context(stop_signal)?;

        let c_url = std::ffi::CString::new(path)?;
        let ret = unsafe { avformat_open_input(&mut ctx, c_url.as_ptr(), null_mut(), null_mut()) };
        if ret < 0 {
            return Err(anyhow!(ffmpeg_error(ret)));
        }

        let ret = unsafe { avformat_find_stream_info(ctx, null_mut()) };
        if ret < 0 {
            unsafe { avformat_close_input(&mut ctx) };
            return Err(anyhow!(ffmpeg_error(ret)));
        }

        Ok(Self { ctx })
    }
}

impl Drop for SrtInputContext {
    fn drop(&mut self) {
        if self.ctx.is_null() {
            return;
        }
        unsafe { avformat_close_input(&mut self.ctx) };
        self.ctx = null_mut();
    }
}

impl Context for SrtInputContext {
    fn get_ctx(&self) -> *mut AVFormatContext {
        self.ctx
    }
}

impl InputContext for SrtInputContext {}

extern "C" fn interrupt_callback(opaque: *mut c_void) -> i32 {
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
