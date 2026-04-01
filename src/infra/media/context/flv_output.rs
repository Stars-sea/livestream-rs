//! FLV output context for streaming to RTMP servers.

use super::{Context, OutputContext};
use crate::infra::media::ffmpeg_error;
use crate::infra::media::stream::StreamCollection;

use anyhow::Result;
use crossfire::{MTx, mpsc};
use ffmpeg_sys_next::*;

use std::ffi::{c_int, c_void};
use std::ptr::null_mut;
use std::sync::Arc;

use bytes::Bytes;

#[derive(Debug, Clone)]
pub enum FlvPacket {
    Data { live_id: String, data: Bytes },
    EndOfStream { live_id: String },
}

impl FlvPacket {
    pub fn live_id(&self) -> &str {
        match self {
            FlvPacket::Data { live_id, .. } => live_id,
            FlvPacket::EndOfStream { live_id } => live_id,
        }
    }
}

/// Wrapper for FFmpeg output context configured for FLV streaming to RTMP servers.
#[derive(Debug)]
pub struct FlvOutputContext {
    ctx: *mut AVFormatContext,
}

impl FlvOutputContext {
    pub fn create(
        live_id: String,
        flv_packet_tx: MTx<mpsc::List<FlvPacket>>,
        input_ctx: &impl Context,
    ) -> Result<Self> {
        let ctx = Self::alloc_output_ctx("flv", None)?;

        if let Err(e) = Self::copy_streams(ctx, input_ctx) {
            unsafe { avformat_free_context(ctx) };
            return Err(e);
        }

        if unsafe { (*ctx).pb.is_null() } {
            let opaque = Arc::new(FlvAvioOpaque {
                live_id,
                flv_packet_tx,
            });
            let opaque_ptr = Arc::into_raw(opaque) as *mut c_void;
            match Self::open_io(opaque_ptr, None, AVIO_FLAG_WRITE) {
                Ok(pb) => unsafe {
                    (*ctx).pb = pb;
                    (*ctx).flags |= AVFMT_FLAG_CUSTOM_IO;
                },
                Err(e) => {
                    unsafe {
                        let _ = Arc::from_raw(opaque_ptr as *const FlvAvioOpaque);
                    }
                    unsafe { avformat_free_context(ctx) };
                    return Err(e);
                }
            }
        }

        if let Err(e) = Self::write_header(ctx) {
            unsafe { Self::cleanup_ctx(ctx) };
            return Err(e);
        }

        Ok(Self { ctx })
    }
}

impl Drop for FlvOutputContext {
    fn drop(&mut self) {
        if self.ctx.is_null() {
            return;
        }

        self.write_trailer().ok();
        unsafe { Self::cleanup_ctx(self.ctx) };
        self.ctx = null_mut();
    }
}

impl Context for FlvOutputContext {
    unsafe fn ptr(&self) -> *mut AVFormatContext {
        self.ctx
    }
}

impl OutputContext for FlvOutputContext {
    fn copy_streams(ctx_ptr: *mut AVFormatContext, streams: &impl StreamCollection) -> Result<()> {
        for i in 0..streams.stream_count() {
            let in_stream = streams.stream(i).unwrap();
            let out_stream = unsafe { avformat_new_stream(ctx_ptr, null_mut()) };
            if out_stream.is_null() {
                anyhow::bail!("Failed to allocate output stream");
            }

            let ret =
                unsafe { avcodec_parameters_copy((*out_stream).codecpar, in_stream.codec_params_ptr()) };

            if ret < 0 {
                anyhow::bail!("Failed to copy streams parameters: {}", ffmpeg_error(ret));
            }

            unsafe {
                (*(*out_stream).codecpar).codec_tag = 0;
                (*out_stream).time_base = in_stream.time_base();
            }
        }

        Ok(())
    }

    fn open_io(
        opaque: *mut c_void,
        _path: Option<&str>,
        _flags: c_int,
    ) -> Result<*mut AVIOContext> {
        let buffer_size = 1024 * 32; // 32KB buffer
        let avio_buffer = unsafe { av_malloc(buffer_size) as *mut u8 };

        let pb = unsafe {
            avio_alloc_context(
                avio_buffer,
                buffer_size as i32,
                1,
                opaque,
                None,
                Some(write_packet),
                None,
            )
        };
        if pb.is_null() {
            unsafe { av_free(avio_buffer as *mut c_void) };
            anyhow::bail!("Failed to allocate I/O context");
        } else {
            Ok(pb)
        }
    }
}

impl FlvOutputContext {
    unsafe fn cleanup_ctx(ctx: *mut AVFormatContext) {
        if ctx.is_null() {
            return;
        }

        let pb = unsafe { (*ctx).pb };
        if !pb.is_null() {
            if unsafe { !(*pb).opaque.is_null() } {
                let _ = unsafe { Arc::from_raw((*pb).opaque as *const FlvAvioOpaque) };
                unsafe { (*pb).opaque = null_mut() };
            }
            unsafe { av_freep(&mut (*pb).buffer as *mut _ as *mut c_void) };
        }

        unsafe { avio_context_free(&mut (*ctx).pb) };
        unsafe { avformat_free_context(ctx) };
    }

    #[allow(dead_code)]
    pub fn get_flv_packet_sender(&self) -> Option<MTx<mpsc::List<FlvPacket>>> {
        if self.ctx.is_null() {
            return None;
        }

        unsafe {
            if (*self.ctx).pb.is_null() {
                return None;
            }

            let opaque_ptr = (*(*self.ctx).pb).opaque as *const FlvAvioOpaque;
            if opaque_ptr.is_null() {
                return None;
            }

            let opaque = &*opaque_ptr;
            Some(opaque.flv_packet_tx.clone())
        }
    }
}

pub struct FlvAvioOpaque {
    live_id: String,
    flv_packet_tx: MTx<mpsc::List<FlvPacket>>,
}

extern "C" fn write_packet(opaque: *mut c_void, buf: *const u8, buf_size: c_int) -> c_int {
    if opaque.is_null() || buf.is_null() || buf_size <= 0 {
        return 0; // Invalid parameters, nothing to write
    }

    // Do not use the Arc here since we only need to read from it and it will be dropped when the context is dropped
    let opaque_ref = unsafe { &*(opaque as *const FlvAvioOpaque) };

    // Convert the raw buffer to a Rust slice and then to a Bytes for sending through the channel
    let data_slice = unsafe { std::slice::from_raw_parts(buf, buf_size as usize) };
    let data = Bytes::copy_from_slice(data_slice);

    // Attempt to send the data to the async channel. If the receiver has been dropped, return EOF.
    match opaque_ref.flv_packet_tx.send(FlvPacket::Data {
        live_id: opaque_ref.live_id.clone(),
        data,
    }) {
        Ok(_) => buf_size,
        Err(_) => AVERROR_EOF,
    }
}
