//! FFmpeg packet wrapper for safe packet operations.

use crate::infra::media::context::{Context, OutputContext};
use crate::infra::media::stream::StreamCollection;
use crate::infra::media::ffmpeg_error;

use anyhow::{Result, anyhow};
use ffmpeg_sys_next::*;

#[derive(Debug)]
pub enum PacketReadResult {
    Data,
    Eof,
    Retryable { code: i32, message: String },
    Fatal { code: i32, message: String },
}

/// Wrapper for FFmpeg AVPacket with safe operations.
///
/// # Safety
/// Manages the lifecycle of AVPacket through RAII.
/// The packet is allocated in `alloc()` and freed in `Drop`.
#[derive(Debug)]
pub struct Packet {
    pub(super) packet: *mut AVPacket,
}

impl Packet {
    /// Allocates a new packet.
    ///
    /// # Errors
    /// Returns an error if allocation fails.
    pub fn alloc() -> Result<Self> {
        let pkt = unsafe { av_packet_alloc() };
        if pkt.is_null() {
            anyhow::bail!("Failed to allocate AVPacket");
        }
        Ok(Self { packet: pkt })
    }

    pub fn read(&mut self, ctx: &impl Context) -> PacketReadResult {
        let ret = unsafe { av_read_frame(ctx.ptr(), self.packet) };

        if ret >= 0 {
            return PacketReadResult::Data;
        }

        if ret == AVERROR_EOF {
            return PacketReadResult::Eof;
        }

        let err_msg = ffmpeg_error(ret);

        if ret == AVERROR(EAGAIN) || ret == AVERROR(ETIMEDOUT) {
            return PacketReadResult::Retryable {
                code: ret,
                message: err_msg,
            };
        }

        PacketReadResult::Fatal {
            code: ret,
            message: err_msg,
        }
    }

    pub fn rescale_ts(&mut self, original_time_base: AVRational, target_time_base: AVRational) {
        unsafe { av_packet_rescale_ts(self.packet, original_time_base, target_time_base) }
    }

    pub fn rescale_ts_for_ctx(
        &mut self,
        in_ctx: &impl Context,
        out_ctx: &impl Context,
    ) -> Result<()> {
        let stream_idx = self.stream_idx();
        let in_stream = in_ctx
            .stream(stream_idx)
            .ok_or_else(|| anyhow::anyhow!("Input stream {} not found", stream_idx))?;
        let out_stream = out_ctx
            .stream(stream_idx)
            .ok_or_else(|| anyhow::anyhow!("Output stream {} not found", stream_idx))?;

        self.rescale_ts(in_stream.time_base(), out_stream.time_base());
        Ok(())
    }

    pub fn write(self, ctx: &impl OutputContext) -> Result<()> {
        if !ctx.available() {
            return Err(anyhow!("Context is not available"));
        }

        let ret = unsafe { av_interleaved_write_frame(ctx.ptr(), self.packet) };
        if ret < 0 {
            Err(anyhow!(
                "av_interleaved_write_frame failed: {}",
                ffmpeg_error(ret)
            ))
        } else {
            Ok(())
        }
    }

    pub fn stream_idx(&self) -> usize {
        unsafe { (*self.packet).stream_index as usize }
    }

    pub fn size(&self) -> i32 {
        unsafe { (*self.packet).size }
    }

    pub fn pts(&self) -> Option<i64> {
        let pts = unsafe { (*self.packet).pts };
        if pts != AV_NOPTS_VALUE {
            Some(pts)
        } else {
            None
        }
    }

    pub fn has_flag(&self, flag: i32) -> bool {
        unsafe { (*self.packet).flags & flag != 0 }
    }

    pub fn is_key_frame(&self) -> bool {
        self.has_flag(AV_PKT_FLAG_KEY)
    }
}

unsafe impl Send for Packet {}
unsafe impl Sync for Packet {}

impl Clone for Packet {
    fn clone(&self) -> Self {
        let pkt_ptr = unsafe { av_packet_clone(self.packet) };
        assert!(
            !pkt_ptr.is_null(),
            "av_packet_clone returned null (allocation failure)"
        );
        Self { packet: pkt_ptr }
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        unsafe { av_packet_free(&mut self.packet) }
    }
}
