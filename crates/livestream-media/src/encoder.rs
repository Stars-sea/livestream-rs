//! Encoder — AVCodecContext RAII wrapper for the encode direction.
//!
//! Used by the `Transcode` processor (Phase 4).

use anyhow::Result;
use ffmpeg_sys_next::*;

use crate::frame::Frame;
use crate::packet::Packet;

/// Wraps `AVCodecContext*` for encoding. Drop calls `avcodec_free_context`.
pub struct Encoder {
    ctx: *mut AVCodecContext,
}

// SAFETY: AVCodecContext is not thread-safe — must be wrapped in a Mutex
// when shared. The struct itself is `Send` for ownership transfer.
unsafe impl Send for Encoder {}

impl Encoder {
    /// Create a new encoder.
    ///
    /// Finds the encoder by codec ID, allocates a context, and configures
    /// basic encoding parameters.
    pub fn new(
        codec_id: AVCodecID,
        width: i32,
        height: i32,
        pix_fmt: AVPixelFormat,
        time_base: AVRational,
        bit_rate: i64,
    ) -> Result<Self> {
        // SAFETY: avcodec_find_encoder returns a static descriptor or NULL.
        let codec = unsafe { avcodec_find_encoder(codec_id) };
        if codec.is_null() {
            anyhow::bail!("Encoder not found for codec ID: {:?}", codec_id);
        }

        // SAFETY: avcodec_alloc_context3 returns a heap-allocated context or NULL.
        let mut ctx = unsafe { avcodec_alloc_context3(codec) };
        if ctx.is_null() {
            anyhow::bail!("Failed to allocate encoder context");
        }

        // SAFETY: ctx is a freshly allocated AVCodecContext. Setting fields
        // before opening is the standard FFmpeg pattern.
        unsafe {
            (*ctx).width = width;
            (*ctx).height = height;
            (*ctx).pix_fmt = pix_fmt;
            (*ctx).time_base = time_base;
            (*ctx).bit_rate = bit_rate;

            // Set a reasonable GOP size.
            (*ctx).gop_size = 60;
            (*ctx).max_b_frames = 0;
        }

        // SAFETY: Context is configured; avcodec_open2 initializes the encoder.
        let ret = unsafe { avcodec_open2(ctx, std::ptr::null(), std::ptr::null_mut()) };
        if ret < 0 {
            // SAFETY: Free context on failure before returning error.
            unsafe {
                avcodec_free_context(&mut ctx);
            }
            anyhow::bail!("Failed to open encoder: {}", crate::ffmpeg_error(ret));
        }

        Ok(Self { ctx })
    }

    /// Send a raw frame to the encoder.
    ///
    /// Pass `None` to flush the encoder (drain remaining packets).
    pub fn send_frame(&mut self, frame: Option<&Frame>) -> Result<()> {
        let frame_ptr = frame.map(|f| f.as_ptr()).unwrap_or(std::ptr::null());
        // SAFETY: frame_ptr is either a valid AVFrame pointer or NULL (flush).
        let ret = unsafe { avcodec_send_frame(self.ctx, frame_ptr) };
        if ret < 0 {
            anyhow::bail!(
                "Failed to send frame to encoder: {}",
                crate::ffmpeg_error(ret)
            );
        }
        Ok(())
    }

    /// Receive an encoded packet from the encoder.
    ///
    /// Returns `Ok(true)` when a packet is available, `Ok(false)` when more
    /// frames are needed (`AVERROR(EAGAIN)`) or the stream ended (`AVERROR_EOF`),
    /// and `Err` on fatal errors.
    pub fn receive_packet(&mut self, pkt: &mut Packet) -> Result<bool> {
        // SAFETY: Both pointers are valid. avcodec_receive_packet fills
        // the packet with encoded data.
        let ret = unsafe { avcodec_receive_packet(self.ctx, pkt.as_mut_ptr()) };
        if ret == AVERROR(EAGAIN) || ret == AVERROR_EOF {
            return Ok(false);
        }
        if ret < 0 {
            anyhow::bail!(
                "Failed to receive packet from encoder: {}",
                crate::ffmpeg_error(ret)
            );
        }
        Ok(true)
    }

    /// Get the encoder's time base.
    pub fn time_base(&self) -> AVRational {
        // SAFETY: reading field from valid AVCodecContext pointer.
        unsafe { (*self.ctx).time_base }
    }

    /// Get the encoder's pixel format.
    pub fn pixel_format(&self) -> AVPixelFormat {
        // SAFETY: reading field from valid AVCodecContext pointer.
        unsafe { (*self.ctx).pix_fmt }
    }

    pub fn width(&self) -> i32 {
        // SAFETY: reading field from valid AVCodecContext pointer.
        unsafe { (*self.ctx).width }
    }

    pub fn height(&self) -> i32 {
        // SAFETY: reading field from valid AVCodecContext pointer.
        unsafe { (*self.ctx).height }
    }
}

impl Drop for Encoder {
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
    use ffmpeg_sys_next::AVPixelFormat;

    #[test]
    fn create_h264_encoder() {
        let enc = Encoder::new(
            AVCodecID::AV_CODEC_ID_H264,
            1920,
            1080,
            AVPixelFormat::AV_PIX_FMT_YUV420P,
            AVRational { num: 1, den: 30 },
            2_000_000,
        );
        assert!(enc.is_ok());
    }

    #[test]
    fn encoder_has_correct_params() {
        let enc = Encoder::new(
            AVCodecID::AV_CODEC_ID_H264,
            1280,
            720,
            AVPixelFormat::AV_PIX_FMT_YUV420P,
            AVRational { num: 1, den: 25 },
            1_000_000,
        )
        .unwrap();
        assert_eq!(enc.width(), 1280);
        assert_eq!(enc.height(), 720);
        assert_eq!(enc.pixel_format(), AVPixelFormat::AV_PIX_FMT_YUV420P);
        assert_eq!(enc.time_base().num, 1);
        assert_eq!(enc.time_base().den, 25);
    }
}
