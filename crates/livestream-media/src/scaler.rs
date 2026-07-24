//! Scaler — SwsContext RAII wrapper for pixel format/scale conversion.
//!
//! Used by the `Transcode` processor (Phase 4).

use anyhow::Result;
use ffmpeg_sys_next::*;

use crate::frame::Frame;

/// Wraps `SwsContext*` for frame scaling/conversion. Drop calls `sws_freeContext`.
pub struct Scaler {
    ctx: *mut SwsContext,
    src_w: i32,
    src_h: i32,
    #[allow(dead_code)]
    src_fmt: AVPixelFormat,
    dst_w: i32,
    dst_h: i32,
    #[allow(dead_code)]
    dst_fmt: AVPixelFormat,
}

// SAFETY: SwsContext is thread-safe for immutable operations. Mutable
// operations (scale) are protected by &mut self.
unsafe impl Send for Scaler {}

impl Scaler {
    /// Create a new scaler.
    ///
    /// Uses `sws_getContext` with the given source/destination parameters.
    pub fn new(
        src_w: i32,
        src_h: i32,
        src_fmt: AVPixelFormat,
        dst_w: i32,
        dst_h: i32,
        dst_fmt: AVPixelFormat,
        flags: i32,
    ) -> Result<Self> {
        // SAFETY: sws_getContext returns a heap-allocated context or NULL.
        let ctx = unsafe {
            sws_getContext(
                src_w,
                src_h,
                src_fmt,
                dst_w,
                dst_h,
                dst_fmt,
                flags,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if ctx.is_null() {
            anyhow::bail!("Failed to create SwsContext");
        }

        Ok(Self {
            ctx,
            src_w,
            src_h,
            src_fmt,
            dst_w,
            dst_h,
            dst_fmt,
        })
    }

    /// Scale `src` frame into `dst` frame.
    ///
    /// The destination frame must be pre-allocated with matching format and
    /// dimensions.
    pub fn scale(&mut self, src: &Frame, dst: &mut Frame) -> Result<()> {
        // SAFETY: All pointers and dimensions are valid. sws_scale_frame
        // performs the scaling in-place on the destination frame.
        let ret = unsafe { sws_scale_frame(self.ctx, dst.as_mut_ptr(), src.as_ptr()) };
        if ret < 0 {
            anyhow::bail!("Failed to scale frame: {}", crate::ffmpeg_error(ret));
        }
        Ok(())
    }

    // ── Dimension accessors ──

    pub fn src_width(&self) -> i32 {
        self.src_w
    }

    pub fn src_height(&self) -> i32 {
        self.src_h
    }

    pub fn dst_width(&self) -> i32 {
        self.dst_w
    }

    pub fn dst_height(&self) -> i32 {
        self.dst_h
    }
}

impl Drop for Scaler {
    fn drop(&mut self) {
        // SAFETY: sws_freeContext frees the context. Safe to call with a
        // valid pointer.
        unsafe {
            sws_freeContext(self.ctx);
        }
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use ffmpeg_sys_next::AVPixelFormat;

    #[test]
    fn create_scaler() {
        let s = Scaler::new(
            1920,
            1080,
            AVPixelFormat::AV_PIX_FMT_YUV420P,
            1280,
            720,
            AVPixelFormat::AV_PIX_FMT_YUV420P,
            SwsFlags::SWS_BICUBIC as i32,
        );
        assert!(s.is_ok());
    }

    #[test]
    fn scaler_dimensions() {
        let s = Scaler::new(
            1920,
            1080,
            AVPixelFormat::AV_PIX_FMT_YUV420P,
            640,
            360,
            AVPixelFormat::AV_PIX_FMT_YUV420P,
            SwsFlags::SWS_FAST_BILINEAR as i32,
        )
        .unwrap();
        assert_eq!(s.src_width(), 1920);
        assert_eq!(s.src_height(), 1080);
        assert_eq!(s.dst_width(), 640);
        assert_eq!(s.dst_height(), 360);
    }
}
