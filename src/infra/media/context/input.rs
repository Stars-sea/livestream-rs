use super::Context;
use crate::infra::media::ffmpeg_error;
use crate::infra::media::options::StreamOptions;

use anyhow::Result;
use ffmpeg_sys_next::*;
use tokio_util::sync::CancellationToken;

use std::ffi::CString;
use std::ptr::null_mut;

/// Unified FFmpeg input context wrapper for different ingest protocols.
#[derive(Debug)]
pub struct InputContext {
    ctx: *mut AVFormatContext,
}

impl InputContext {
    pub fn open(options: &impl StreamOptions, cancel_token: CancellationToken) -> Result<Self> {
        let mut ctx = alloc_input_context(Box::new(cancel_token))?;
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

unsafe impl Send for InputContext {}

impl Drop for InputContext {
    fn drop(&mut self) {
        free_input_context(self.ctx);
        self.ctx = null_mut();
    }
}

impl Context for InputContext {
    unsafe fn ptr(&self) -> *mut AVFormatContext {
        self.ctx
    }
}

fn alloc_input_context(cancel_token: Box<CancellationToken>) -> Result<*mut AVFormatContext> {
    let ctx = unsafe { avformat_alloc_context() };
    if ctx.is_null() {
        return Err(anyhow::anyhow!("Failed to alloc AVFormatContext"));
    }

    unsafe {
        let cancel_token_ptr = Box::into_raw(cancel_token) as *mut std::ffi::c_void;
        (*ctx).interrupt_callback = AVIOInterruptCB {
            callback: Some(interrupt_callback),
            opaque: cancel_token_ptr,
        };
    }

    Ok(ctx)
}

fn free_input_context(ctx: *mut AVFormatContext) {
    if ctx.is_null() {
        return;
    }
    unsafe {
        let opaque = (*ctx).interrupt_callback.opaque;
        if !opaque.is_null() {
            let _ = Box::from_raw(opaque as *mut CancellationToken);
        }
        avformat_close_input(&mut { ctx });
    }
}

extern "C" fn interrupt_callback(opaque: *mut std::ffi::c_void) -> i32 {
    if opaque.is_null() {
        return 0;
    }
    let cancel_token = unsafe { &*(opaque as *mut CancellationToken) };
    if cancel_token.is_cancelled() { 0 } else { 1 }
}
