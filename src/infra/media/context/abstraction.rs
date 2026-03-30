//! Context trait and utilities for FFmpeg format contexts.

use crate::infra::media::{StreamTrait, ffmpeg_error};

use anyhow::Result;
use ffmpeg_sys_next::*;

use std::ffi::{CString, c_int, c_void};
use std::ptr::{null, null_mut};

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
    fn stream(&self, id: u32) -> Option<*mut AVStream> {
        if id < self.nb_streams() {
            let ptr = unsafe { (*self.get_ctx()).streams.offset(id as isize) };
            unsafe { Some(*ptr) }
        } else {
            None
        }
    }

    /// Checks if the context is available (pointer is not null).
    fn available(&self) -> bool {
        !self.get_ctx().is_null()
    }
}

pub trait OutputContext: Context {
    /// Copies stream parameters from an input context to this output context.
    fn copy_streams(ctx_ptr: *mut AVFormatContext, input_ctx: &impl Context) -> Result<()> {
        for i in 0..input_ctx.nb_streams() {
            let in_stream = input_ctx
                .stream(i)
                .ok_or_else(|| anyhow::anyhow!("Stream not found in input context"))?;
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
        }

        Ok(())
    }

    /// Allocates an output context for the specified format and URL.
    fn alloc_output_ctx(format: &str, url: Option<&str>) -> Result<*mut AVFormatContext> {
        let mut ctx: *mut AVFormatContext = null_mut();
        let c_format = CString::new(format)?;
        let c_filename = url.map(|u| CString::new(u)).transpose()?;

        let ret = unsafe {
            let filename = match &c_filename {
                Some(c) => c.as_ptr(),
                None => null(),
            };
            avformat_alloc_output_context2(&mut ctx, null_mut(), c_format.as_ptr(), filename)
        };
        if ret < 0 {
            anyhow::bail!("Failed allocate output context: {}", ffmpeg_error(ret));
        } else {
            Ok(ctx)
        }
    }

    fn open_io(_opaque: *mut c_void, path: Option<&str>, flags: c_int) -> Result<*mut AVIOContext> {
        let mut pb: *mut AVIOContext = null_mut();
        let c_path = path.map(|p| CString::new(p)).transpose()?;

        let ret = unsafe {
            let path = match &c_path {
                Some(c) => c.as_ptr(),
                None => null(),
            };
            avio_open(&mut pb, path, flags)
        };
        if ret < 0 {
            anyhow::bail!("Failed to open I/O context: {}", ffmpeg_error(ret));
        } else {
            Ok(pb)
        }
    }

    /// Writes the header for the output context.
    fn write_header(ctx: *mut AVFormatContext) -> Result<()> {
        let ret = unsafe { avformat_write_header(ctx, null_mut()) };
        if ret < 0 {
            anyhow::bail!("Failed to write header: {}", ffmpeg_error(ret));
        } else {
            Ok(())
        }
    }

    fn write_trailer(&self) -> Result<()> {
        let ret = unsafe { av_write_trailer(self.get_ctx()) };
        if ret < 0 {
            anyhow::bail!("Failed to write trailer: {}", ffmpeg_error(ret));
        } else {
            Ok(())
        }
    }
}
