//! FLV output context for streaming to RTMP servers.

use anyhow::Result;
use bytes::{Buf, BytesMut};
use ffmpeg_sys_next::*;
use parking_lot::Mutex;
use tracing::warn;

use std::ffi::{c_int, c_void};
use std::ptr::null_mut;

use super::{Context, OutputContext};
use crate::channel::{MpscTx, SendError};
use crate::infra::media::ffmpeg_error;
use crate::infra::media::packet::FlvTag;
use crate::infra::media::stream::StreamCollection;

const FLV_HEADER_AND_PREV_TAG_SIZE_LEN: usize = 13;
const FLV_TAG_HEADER_LEN: usize = 11;
const FLV_PREV_TAG_SIZE_LEN: usize = 4;
const FLV_MIN_TAG_BLOCK_LEN: usize = FLV_TAG_HEADER_LEN + FLV_PREV_TAG_SIZE_LEN;
const FLV_MAX_BUFFERED_BYTES: usize = 4 * 1024 * 1024;

/// Wrapper for FFmpeg output context configured for FLV streaming to RTMP servers.
#[derive(Debug)]
pub struct FlvOutputContext {
    ctx: *mut AVFormatContext,
}

impl FlvOutputContext {
    pub fn create(flv_tag_tx: MpscTx<FlvTag>, streams: &dyn StreamCollection) -> Result<Self> {
        let ctx = Self::alloc_output_ctx("flv", None)?;

        if let Err(e) = Self::copy_streams(ctx, streams) {
            unsafe { avformat_free_context(ctx) };
            return Err(e);
        }

        if unsafe { (*ctx).pb.is_null() } {
            let opaque = Box::new(FlvAvioOpaque {
                flv_tag_tx,
                write_state: Mutex::new(FlvWriteState::default()),
            });
            let opaque_ptr = Box::into_raw(opaque) as *mut c_void;
            match Self::open_io(opaque_ptr, None, AVIO_FLAG_WRITE) {
                Ok(pb) => unsafe {
                    (*ctx).pb = pb;
                    (*ctx).flags |= AVFMT_FLAG_CUSTOM_IO;
                },
                Err(e) => {
                    unsafe {
                        let _ = Box::from_raw(opaque_ptr as *mut FlvAvioOpaque);
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

    unsafe fn cleanup_ctx(ctx: *mut AVFormatContext) {
        if ctx.is_null() {
            return;
        }

        let pb = unsafe { (*ctx).pb };
        if !pb.is_null() {
            if unsafe { !(*pb).opaque.is_null() } {
                let _ = unsafe { Box::from_raw((*pb).opaque as *mut FlvAvioOpaque) };
                unsafe { (*pb).opaque = null_mut() };
            }
            unsafe { av_freep(&mut (*pb).buffer as *mut _ as *mut c_void) };
        }

        unsafe { avio_context_free(&mut (*ctx).pb) };
        unsafe { avformat_free_context(ctx) };
    }
}

unsafe impl Send for FlvOutputContext {}

impl Drop for FlvOutputContext {
    fn drop(&mut self) {
        if self.ctx.is_null() {
            return;
        }

        if let Err(e) = self.write_trailer() {
            warn!(error = %e, "Failed to write FLV trailer during context drop");
        }
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
    fn copy_streams(ctx_ptr: *mut AVFormatContext, streams: &dyn StreamCollection) -> Result<()> {
        for in_stream in streams {
            let out_stream = unsafe { avformat_new_stream(ctx_ptr, null_mut()) };
            if out_stream.is_null() {
                anyhow::bail!("Failed to allocate output stream");
            }

            let ret = unsafe {
                avcodec_parameters_copy((*out_stream).codecpar, in_stream.codec_params_ptr())
            };

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

struct FlvAvioOpaque {
    flv_tag_tx: MpscTx<FlvTag>,
    write_state: Mutex<FlvWriteState>,
}

#[derive(Default)]
struct FlvWriteState {
    buffered: BytesMut,
    header_consumed: bool,
}

fn parse_ready_flv_tags(state: &mut FlvWriteState) -> Result<Vec<FlvTag>, c_int> {
    let mut parsed_tags = Vec::new();

    while state.buffered.len() >= FLV_MIN_TAG_BLOCK_LEN {
        let tag_type = state.buffered[0];
        if tag_type != 8 && tag_type != 9 && tag_type != 18 {
            warn!(
                tag_type = tag_type,
                "Unexpected FLV tag type in mux output buffer"
            );
            state.buffered.clear();
            return Err(AVERROR(EINVAL));
        }

        let data_size = ((state.buffered[1] as usize) << 16)
            | ((state.buffered[2] as usize) << 8)
            | (state.buffered[3] as usize);
        let expected_len = FLV_TAG_HEADER_LEN + data_size + FLV_PREV_TAG_SIZE_LEN;

        if state.buffered.len() < expected_len {
            break;
        }

        let tag_bytes = state.buffered.split_to(expected_len).freeze();
        let tag = match FlvTag::try_from(tag_bytes) {
            Ok(tag) => tag,
            Err(e) => {
                warn!(error = %e, "Failed to parse FLV tag from mux output buffer");
                state.buffered.clear();
                return Err(AVERROR(EINVAL));
            }
        };

        parsed_tags.push(tag);
    }

    Ok(parsed_tags)
}

extern "C" fn write_packet(opaque: *mut c_void, buf: *const u8, buf_size: c_int) -> c_int {
    if opaque.is_null() || buf.is_null() || buf_size <= 0 {
        return 0; // Invalid parameters, nothing to write
    }

    // Opaque memory is owned by AVIO context and released in cleanup_ctx.
    let opaque_ref = unsafe { &*(opaque as *const FlvAvioOpaque) };

    // Buffer raw bytes because FFmpeg callback boundaries may split or merge FLV tags.
    let data_slice = unsafe { std::slice::from_raw_parts(buf, buf_size as usize) };

    let parse_result = {
        let mut state = opaque_ref.write_state.lock();

        state.buffered.extend_from_slice(data_slice);

        if state.buffered.len() > FLV_MAX_BUFFERED_BYTES {
            warn!(
                size = state.buffered.len(),
                "FLV mux output buffer exceeded safety limit"
            );
            state.buffered.clear();
            return AVERROR(EINVAL);
        }

        if !state.header_consumed {
            if state.buffered.len() < FLV_HEADER_AND_PREV_TAG_SIZE_LEN {
                Ok(Vec::new())
            } else {
                if state.buffered.starts_with(b"FLV") {
                    state.buffered.advance(FLV_HEADER_AND_PREV_TAG_SIZE_LEN);
                }
                state.header_consumed = true;
                parse_ready_flv_tags(&mut state)
            }
        } else {
            parse_ready_flv_tags(&mut state)
        }
    };

    let tags_to_send = match parse_result {
        Ok(tags) => tags,
        Err(code) => return code,
    };

    for tag in tags_to_send {
        if matches!(opaque_ref.flv_tag_tx.send(tag), Err(SendError::Closed)) {
            return AVERROR_EOF;
        }
    }

    buf_size
}
