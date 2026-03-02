use super::context::Context;
use super::ffmpeg_error;
use super::options::StreamOptions;

use anyhow::Result;
use ffmpeg_sys_next::*;

use std::ffi::CString;
use std::ptr::null_mut;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Unified FFmpeg input context wrapper for different ingest protocols.
#[derive(Debug)]
pub struct InputContext {
    ctx: *mut AVFormatContext,
}

impl InputContext {
    pub fn open(options: &impl StreamOptions, stop_signal: Arc<AtomicBool>) -> Result<Self> {
        let mut ctx = alloc_input_context(stop_signal)?;
        let url = options.filename();
        let c_url = CString::new(url.as_str())?;
        let mut c_opt = options.to_dict();

        let ret = unsafe { avformat_open_input(&mut ctx, c_url.as_ptr(), null_mut(), &mut c_opt) };

        if !c_opt.is_null() {
            unsafe { av_dict_free(&mut c_opt) };
        }

        if ret < 0 {
            let err_msg = ffmpeg_error(ret);
            free_input_context(ctx);
            anyhow::bail!("Failed to open input {}: {}", url, err_msg);
        }

        let ret = unsafe { avformat_find_stream_info(ctx, null_mut()) };
        if ret < 0 {
            let err_msg = ffmpeg_error(ret);
            free_input_context(ctx);
            anyhow::bail!("Failed to find stream info: {}", err_msg);
        }

        Ok(Self { ctx })
    }
}

impl Drop for InputContext {
    fn drop(&mut self) {
        free_input_context(self.ctx);
        self.ctx = null_mut();
    }
}

impl Context for InputContext {
    fn get_ctx(&self) -> *mut AVFormatContext {
        self.ctx
    }
}

fn alloc_input_context(stop_signal: Arc<AtomicBool>) -> Result<*mut AVFormatContext> {
    let ctx = unsafe { avformat_alloc_context() };
    if ctx.is_null() {
        return Err(anyhow::anyhow!("Failed to alloc AVFormatContext"));
    }

    unsafe {
        let stop_signal_ptr = Arc::into_raw(stop_signal) as *mut std::ffi::c_void;
        (*ctx).interrupt_callback = AVIOInterruptCB {
            callback: Some(interrupt_callback),
            opaque: stop_signal_ptr,
        };
    }

    Ok(ctx)
}

fn free_input_context(ctx: *mut AVFormatContext) {
    if ctx.is_null() {
        return;
    }
    unsafe {
        if !(*ctx).interrupt_callback.opaque.is_null() {
            let _ = Arc::from_raw((*ctx).interrupt_callback.opaque as *const AtomicBool);
        }
        avformat_close_input(&mut { ctx });
    }
}

extern "C" fn interrupt_callback(opaque: *mut std::ffi::c_void) -> i32 {
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
