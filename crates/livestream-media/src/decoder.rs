//! Decoder — AVCodecContext RAII wrapper for the decode direction.
//!
//! Used by the `Transcode` processor (Phase 4).

use anyhow::Result;
use ffmpeg_sys_next::*;

use crate::codec::{CodecParamsPtrTrait, OwnedCodecParams};
use crate::frame::Frame;
use crate::packet::Packet;

/// Wraps `AVCodecContext*` for decoding. Drop calls `avcodec_free_context`.
pub struct Decoder {
    ctx: *mut AVCodecContext,
}

// SAFETY: AVCodecContext is not thread-safe — it must be wrapped in a Mutex
// when shared across tasks. The struct itself is `Send` for ownership transfer.
unsafe impl Send for Decoder {}

impl Decoder {
    /// Create a new decoder for the given codec ID.
    ///
    /// Finds the codec via `avcodec_find_decoder` and allocates a context.
    /// Call `open()` to configure the decoder with actual codec parameters.
    pub fn new(codec_id: AVCodecID) -> Result<Self> {
        // SAFETY: avcodec_find_decoder returns a static descriptor or NULL.
        let codec = unsafe { avcodec_find_decoder(codec_id) };
        if codec.is_null() {
            anyhow::bail!("Decoder not found for codec ID: {:?}", codec_id);
        }

        // SAFETY: avcodec_alloc_context3 returns a heap-allocated context or NULL.
        let ctx = unsafe { avcodec_alloc_context3(codec) };
        if ctx.is_null() {
            anyhow::bail!("Failed to allocate decoder context");
        }

        Ok(Self { ctx })
    }

    /// Open the decoder with the given codec parameters.
    ///
    /// Copies parameters into the decoder context and initializes it.
    pub fn open(&mut self, params: &OwnedCodecParams) -> Result<()> {
        // SAFETY: Both pointers are valid. avcodec_parameters_to_context copies
        // codec parameters into the decoder context.
        let ret = unsafe {
            // SAFETY: CodecParamsPtrTrait::ptr() returns a valid pointer.
            avcodec_parameters_to_context(self.ctx, params.ptr())
        };
        if ret < 0 {
            anyhow::bail!(
                "Failed to copy parameters to decoder: {}",
                crate::ffmpeg_error(ret)
            );
        }

        // SAFETY: Context is configured; avcodec_open2 initializes the decoder.
        let ret = unsafe { avcodec_open2(self.ctx, std::ptr::null(), std::ptr::null_mut()) };
        if ret < 0 {
            anyhow::bail!("Failed to open decoder: {}", crate::ffmpeg_error(ret));
        }

        Ok(())
    }

    /// Send a compressed packet to the decoder.
    ///
    /// Returns `Ok(())` on success, `Err` on fatal error.
    pub fn send_packet(&mut self, pkt: &Packet) -> Result<()> {
        // SAFETY: Both pointers are valid. avcodec_send_packet takes ownership
        // of the packet reference.
        let ret = unsafe { avcodec_send_packet(self.ctx, pkt.as_ptr()) };
        if ret < 0 {
            anyhow::bail!(
                "Failed to send packet to decoder: {}",
                crate::ffmpeg_error(ret)
            );
        }
        Ok(())
    }

    /// Receive a decoded frame from the decoder.
    ///
    /// Returns `Ok(true)` when a frame is available, `Ok(false)` when more
    /// packets are needed (`AVERROR(EAGAIN)`) or the stream ended (`AVERROR_EOF`),
    /// and `Err` on fatal errors.
    pub fn receive_frame(&mut self, frame: &mut Frame) -> Result<bool> {
        // SAFETY: Both pointers are valid. avcodec_receive_frame fills the
        // frame with decoded data (av_frame_unref is called internally first).
        let ret = unsafe { avcodec_receive_frame(self.ctx, frame.as_mut_ptr()) };
        if ret == AVERROR(EAGAIN) || ret == AVERROR_EOF {
            return Ok(false);
        }
        if ret < 0 {
            anyhow::bail!(
                "Failed to receive frame from decoder: {}",
                crate::ffmpeg_error(ret)
            );
        }
        Ok(true)
    }

    /// Get the decoder's time base.
    pub fn time_base(&self) -> AVRational {
        // SAFETY: reading field from valid AVCodecContext pointer.
        unsafe { (*self.ctx).time_base }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: avcodec_free_context frees the context and NULLs the pointer.
        unsafe {
            avcodec_free_context(&mut self.ctx);
        }
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_h264_decoder() {
        let dec = Decoder::new(AVCodecID::AV_CODEC_ID_H264);
        assert!(dec.is_ok());
    }

    #[test]
    fn create_unknown_codec_fails() {
        // AV_CODEC_ID_NONE or an invalid ID should fail.
        let dec = Decoder::new(AVCodecID::AV_CODEC_ID_NONE);
        assert!(dec.is_err());
    }
}
