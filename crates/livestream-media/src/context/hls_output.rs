//! MPEG-TS output context wrapper for HLS segment production.
//!
//! **Key design**: writes TS data to an **in-memory buffer** via custom AVIO
//! callback, not to disk. The caller (HlsSegmenter, Phase 4) flushes the buffer
//! to disk during segment rollover.
//!
//! # Safety
//!
//! The `opaque` pointer passed to `create()` must point to a `Vec<u8>` whose
//! lifetime exceeds the `HlsOutputContext`. The caller guarantees this by
//! Rust's drop order (opaque lives in the same struct as hls_ctx).

use anyhow::Result;
use ffmpeg_sys_next::*;
use tracing::warn;

use std::ffi::{c_int, c_void};
use std::ptr::null_mut;

use super::{Context, OutputContext};
use crate::stream::StreamCollection;

/// Wraps `AVFormatContext` + custom AVIO for in-memory TS muxing.
pub struct HlsOutputContext {
    ctx: *mut AVFormatContext,
    header_written: bool,
}

// SAFETY: AVFormatContext is thread-safe for write operations.
// The opaque pointer (Vec<u8>) is owned by the caller, not this context.
unsafe impl Send for HlsOutputContext {}

impl HlsOutputContext {
    /// Create a TS muxer that writes to an in-memory `Vec<u8>` via custom AVIO.
    ///
    /// # Safety
    ///
    /// `opaque` must point to a `Vec<u8>` whose lifetime exceeds this context.
    /// The caller (HlsSegmenter) guarantees this by storing both in the same
    /// struct — Rust's drop order ensures the buffer outlives the context.
    pub unsafe fn create(streams: &dyn StreamCollection, opaque: *mut c_void) -> Result<Self> {
        // SAFETY: We own the output context allocation — free on error below.
        let ctx = unsafe { Self::alloc_output_ctx("mpegts", None) }?;

        // SAFETY: ctx was just allocated above; copy_streams writes to it.
        if let Err(e) = unsafe { Self::copy_streams(ctx, streams) } {
            // SAFETY: ctx was just allocated above; free it on error.
            unsafe {
                avformat_free_context(ctx);
            }
            return Err(e);
        }

        // Set up custom AVIO → memory buffer.
        // SAFETY: We're attaching a custom AVIO context that writes to the
        // opaque Vec<u8>. The buffer is released in Drop.
        let buffer_size = 1024 * 64; // 64 KB internal FFmpeg buffer
        let avio_buffer = unsafe { av_malloc(buffer_size) as *mut u8 };
        if avio_buffer.is_null() {
            // SAFETY: ctx was allocated above; free it on error.
            unsafe {
                avformat_free_context(ctx);
            }
            anyhow::bail!("Failed to allocate AVIO buffer ({} bytes)", buffer_size);
        }

        let pb = unsafe {
            avio_alloc_context(
                avio_buffer,
                buffer_size as i32,
                1,                     // writable
                opaque,                // → &mut Vec<u8>
                None,                  // no read callback
                Some(ts_write_packet), // write callback
                None,                  // no seek callback
            )
        };
        if pb.is_null() {
            // SAFETY: avio_alloc_context failed — free the buffer we allocated
            // (avio_alloc_context does NOT take ownership on failure).
            unsafe {
                av_free(avio_buffer as *mut c_void);
                avformat_free_context(ctx);
            }
            anyhow::bail!("Failed to allocate AVIO context");
        }

        unsafe {
            (*ctx).pb = pb;
            (*ctx).flags |= AVFMT_FLAG_CUSTOM_IO;
        }

        Ok(Self {
            ctx,
            header_written: false,
        })
    }

    /// Write the TS file header.
    pub fn write_header(&mut self) -> Result<()> {
        // SAFETY: self.ctx is a valid, initialized AVFormatContext.
        let res = unsafe { <Self as OutputContext>::write_header(self.ctx) };
        if res.is_ok() {
            self.header_written = true;
        }
        res
    }

    /// Write an interleaved frame into the TS muxer.
    ///
    /// The packet's timestamps must already be in the muxer's timebase
    /// (typically `{1, 90000}`). Caller must rescale from ms before calling.
    pub fn write_frame(&self, pkt: &crate::packet::Packet) -> Result<()> {
        // SAFETY: Both pointers are valid. av_interleaved_write_frame writes
        // an interleaved packet to the muxer output.
        // SAFETY: pkt is a valid AVPacket. FFmpeg may write back dts after
        // interleaving, so we pass a mutable pointer (C API convention).
        let ret = unsafe { av_interleaved_write_frame(self.ctx, pkt.as_ptr() as *mut AVPacket) };
        if ret < 0 {
            anyhow::bail!(
                "av_interleaved_write_frame failed: {}",
                crate::ffmpeg_error(ret)
            );
        }
        Ok(())
    }

    /// Write the TS file trailer (must be called before segment rollover).
    pub fn write_trailer(&self) -> Result<()> {
        <Self as OutputContext>::write_trailer(self)
    }
}

// ── Custom AVIO write callback ──

/// FFmpeg AVIO write callback — appends TS muxer output to the opaque `Vec<u8>`.
///
/// Returns the number of bytes written on success, or a negative AVERROR code
/// on failure.
extern "C" fn ts_write_packet(opaque: *mut c_void, buf: *const u8, buf_size: c_int) -> c_int {
    if opaque.is_null() || buf.is_null() || buf_size <= 0 {
        return AVERROR(EINVAL);
    }
    // SAFETY: opaque is a valid &mut Vec<u8> pointer provided by the caller.
    // The Vec outlives this context (guaranteed by drop order in HlsSegmenter).
    let buffer: &mut Vec<u8> = unsafe { &mut *(opaque as *mut Vec<u8>) };
    // SAFETY: buf is a valid byte array of length buf_size (provided by FFmpeg).
    let data = unsafe { std::slice::from_raw_parts(buf, buf_size as usize) };
    buffer.extend_from_slice(data);
    buf_size
}

// ── Drop ──

impl Drop for HlsOutputContext {
    fn drop(&mut self) {
        if self.ctx.is_null() {
            return;
        }

        // Write trailer as safety net (normal path: HlsSegmenter calls
        // write_trailer() during rollover).
        if self.header_written
            && let Err(e) = self.write_trailer()
        {
            warn!(error = %e, "Failed to write HLS trailer during context drop");
        }

        // SAFETY: Rule B from ownership map — release in reverse order:
        // 1. AVIO buffer (av_freep)
        // 2. AVIOContext (avio_context_free)
        // 3. AVFormatContext (avformat_free_context)
        unsafe {
            if !(*self.ctx).pb.is_null() {
                av_freep(&mut (*(*self.ctx).pb).buffer as *mut _ as *mut c_void);
                avio_context_free(&mut (*self.ctx).pb);
            }
            avformat_free_context(self.ctx);
        }
        self.ctx = null_mut();
    }
}

impl Context for HlsOutputContext {
    unsafe fn ptr(&self) -> *mut AVFormatContext {
        self.ctx
    }
}

impl OutputContext for HlsOutputContext {}
