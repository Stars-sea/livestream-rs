//! FFmpeg format context traits.
//!
//! `Context` provides access to an `AVFormatContext` pointer.
//! `OutputContext` extends it with write operations for muxers.

mod hls_output;

pub use hls_output::HlsOutputContext;

use anyhow::Result;
use ffmpeg_sys_next::*;

use crate::stream::{StreamCollection, StreamDescriptorTrait};

use std::ffi::{CString, c_int, c_void};
use std::ptr::{null, null_mut};

/// Trait for accessing an `AVFormatContext` pointer.
///
/// # Safety
///
/// Implementors must ensure the pointer returned by `ptr()` is valid
/// for the lifetime of the implementor.
pub trait Context {
    /// Returns the underlying `AVFormatContext` pointer.
    ///
    /// # Safety
    ///
    /// The pointer must remain valid for the lifetime of `self`.
    unsafe fn ptr(&self) -> *mut AVFormatContext;

    /// Whether the context pointer is non-null.
    fn available(&self) -> bool {
        // SAFETY: ptr() is safe to call; caller's responsibility per trait contract.
        !(unsafe { self.ptr() }).is_null()
    }
}

/// Trait for output (muxer) format contexts.
pub trait OutputContext: Context {
    /// Copy a single input stream descriptor to a new output stream.
    ///
    /// # Safety
    ///
    /// `ctx_ptr` must be a valid, non-null `AVFormatContext` pointer.
    unsafe fn copy_stream(
        ctx_ptr: *mut AVFormatContext,
        in_stream: &dyn StreamDescriptorTrait,
    ) -> Result<*mut AVStream> {
        // SAFETY: ctx_ptr is a valid AVFormatContext. avformat_new_stream
        // adds a new stream to the output format.
        let out_stream = unsafe { avformat_new_stream(ctx_ptr, null_mut()) };
        if out_stream.is_null() {
            anyhow::bail!("Failed to allocate output stream");
        }

        // SAFETY: Both pointers are valid. avcodec_parameters_copy deep-copies
        // codec parameters from input to output stream.
        let ret = unsafe {
            avcodec_parameters_copy((*out_stream).codecpar, in_stream.codec_params_ptr())
        };
        if ret < 0 {
            anyhow::bail!(
                "Failed to copy stream parameters: {}",
                crate::ffmpeg_error(ret)
            );
        }

        Ok(out_stream)
    }

    /// Copy all streams from a `StreamCollection` to the output context.
    ///
    /// # Safety
    ///
    /// `ctx_ptr` must be a valid, non-null `AVFormatContext` pointer.
    unsafe fn copy_streams(
        ctx_ptr: *mut AVFormatContext,
        streams: &dyn StreamCollection,
    ) -> Result<()> {
        for in_stream in streams {
            // SAFETY: ctx_ptr is valid per caller's contract.
            unsafe { Self::copy_stream(ctx_ptr, in_stream.as_ref())? };
        }
        Ok(())
    }

    /// Allocate an `AVFormatContext` for the specified output format.
    ///
    /// # Safety
    ///
    /// The returned pointer must be freed with `avformat_free_context`.
    unsafe fn alloc_output_ctx(format: &str, url: Option<&str>) -> Result<*mut AVFormatContext> {
        let mut ctx: *mut AVFormatContext = null_mut();
        let c_format = CString::new(format)?;
        let c_filename = url.map(CString::new).transpose()?;

        // SAFETY: avformat_alloc_output_context2 allocates a new context.
        let ret = unsafe {
            let filename = match &c_filename {
                Some(c) => c.as_ptr(),
                None => null(),
            };
            avformat_alloc_output_context2(&mut ctx, null_mut(), c_format.as_ptr(), filename)
        };
        if ret < 0 {
            anyhow::bail!(
                "Failed to allocate output context: {}",
                crate::ffmpeg_error(ret)
            );
        } else {
            Ok(ctx)
        }
    }

    /// Open I/O for the output context.
    ///
    /// Default: opens a file I/O context. Override for custom AVIO (e.g., HLS memory buffer).
    fn open_io(_opaque: *mut c_void, path: Option<&str>, flags: c_int) -> Result<*mut AVIOContext> {
        let mut pb: *mut AVIOContext = null_mut();
        let c_path = path.map(CString::new).transpose()?;

        // SAFETY: avio_open opens a file I/O context.
        let ret = unsafe {
            let path = match &c_path {
                Some(c) => c.as_ptr(),
                None => null(),
            };
            avio_open(&mut pb, path, flags)
        };
        if ret < 0 {
            anyhow::bail!("Failed to open I/O context: {}", crate::ffmpeg_error(ret));
        } else {
            Ok(pb)
        }
    }

    /// Write the format header.
    ///
    /// # Safety
    ///
    /// `ctx` must be a valid, non-null `AVFormatContext` pointer.
    unsafe fn write_header(ctx: *mut AVFormatContext) -> Result<()> {
        // SAFETY: ctx is a valid AVFormatContext.
        let ret = unsafe { avformat_write_header(ctx, null_mut()) };
        if ret < 0 {
            anyhow::bail!("Failed to write header: {}", crate::ffmpeg_error(ret));
        } else {
            Ok(())
        }
    }

    /// Write the format trailer.
    fn write_trailer(&self) -> Result<()> {
        // SAFETY: self.ptr() returns a valid pointer per trait contract.
        let ret = unsafe { av_write_trailer(self.ptr()) };
        if ret < 0 {
            anyhow::bail!("Failed to write trailer: {}", crate::ffmpeg_error(ret));
        } else {
            Ok(())
        }
    }
}
