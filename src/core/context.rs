//! Context trait and utilities for FFmpeg format contexts.

use crate::core::stream::Stream;
use anyhow::{Result, anyhow};
use ffmpeg_sys_next::*;

use std::ffi::{CStr, CString, c_int};
use std::ptr::null_mut;

/// Trait for accessing AVFormatContext functionality.
///
/// # Safety
/// Implementors must ensure the context pointer is valid and properly managed.
#[allow(drop_bounds)]
pub(crate) trait Context: Drop {
    /// Returns the underlying AVFormatContext pointer.
    ///
    /// # Safety
    /// The pointer must remain valid for the lifetime of the implementor.
    fn get_ctx(&self) -> *mut AVFormatContext;

    /// Returns the number of streams in the context.
    fn nb_streams(&self) -> u32 {
        unsafe { (*self.get_ctx()).nb_streams }
    }

    /// Gets a stream by index.
    ///
    /// # Arguments
    /// * `id` - Stream index (must be < nb_streams())
    ///
    /// # Returns
    /// Some(Stream) if the index is valid, None otherwise.
    fn stream(&self, id: u32) -> Option<Stream> {
        if id < self.nb_streams() {
            let ptr = unsafe { (*self.get_ctx()).streams.offset(id as isize) };
            unsafe { Some(Stream::new(*ptr)) }
        } else {
            None
        }
    }

    /// Checks if the context is available (pointer is not null).
    fn available(&self) -> bool {
        !self.get_ctx().is_null()
    }
}

pub(crate) trait InputContext: Context {}

pub(crate) trait OutputContext: Context {
    /// Copies stream parameters from an input context to this output context.
    fn copy_parameters(ctx_ptr: *mut AVFormatContext, input_ctx: &impl InputContext) -> Result<()> {
        for i in 0..input_ctx.nb_streams() {
            let in_stream = input_ctx.stream(i).unwrap();
            let out_stream = unsafe { avformat_new_stream(ctx_ptr, null_mut()) };
            if out_stream.is_null() {
                unsafe { avformat_free_context(ctx_ptr) };
                return Err(anyhow!("Failed to allocate output stream"));
            }

            let ret = unsafe {
                avcodec_parameters_copy((*out_stream).codecpar, in_stream.codec_params())
            };
            if ret < 0 {
                unsafe { avformat_free_context(ctx_ptr) };
                return Err(anyhow!(
                    "Failed to copy streams parameters: {}",
                    ffmpeg_error(ret)
                ));
            }
        }

        Ok(())
    }

    /// Allocates an output context for the specified format and URL.
    fn alloc_output_ctx(format: &str, url: &str) -> Result<*mut AVFormatContext> {
        let mut ctx: *mut AVFormatContext = null_mut();
        let c_format = CString::new(format)?;
        let c_filename = CString::new(url)?;

        let ret = unsafe {
            avformat_alloc_output_context2(
                &mut ctx,
                null_mut(),
                c_format.as_ptr(),
                c_filename.as_ptr(),
            )
        };
        if ret < 0 {
            Err(anyhow!(
                "Failed allocate output context: {}",
                ffmpeg_error(ret)
            ))
        } else {
            Ok(ctx)
        }
    }

    /// Writes the header for the output context.
    fn write_header(ctx: *mut AVFormatContext) -> Result<()> {
        let ret = unsafe { avformat_write_header(ctx, null_mut()) };
        if ret < 0 {
            unsafe {
                avio_closep(&mut (*ctx).pb);
                avformat_free_context(ctx);
            }
            Err(anyhow!("Failed to write header: {}", ffmpeg_error(ret)))
        } else {
            Ok(())
        }
    }

    fn write_trailer(&self) -> Result<()> {
        let ret = unsafe { av_write_trailer(self.get_ctx()) };
        if ret < 0 {
            Err(anyhow!("Failed to write trailer: {}", ffmpeg_error(ret)))
        } else {
            Ok(())
        }
    }
}

/// Converts an FFmpeg error code to a human-readable string.
///
/// # Arguments
/// * `code` - FFmpeg error code (negative value)
///
/// # Returns
/// Error message string
pub(super) fn ffmpeg_error(code: c_int) -> String {
    let mut buf = [0i8; 1024];
    unsafe {
        av_strerror(code, buf.as_mut_ptr(), buf.len());

        CStr::from_ptr(buf.as_mut_ptr())
            .to_string_lossy()
            .into_owned()
    }
}
