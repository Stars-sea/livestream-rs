//! AVFrame RAII wrapper.
//!
//! `Frame` wraps `*mut AVFrame`. Drop calls `av_frame_free`.

use anyhow::Result;
use ffmpeg_sys_next::*;

/// Wrapper for FFmpeg `AVFrame` with RAII cleanup.
pub struct Frame {
    ptr: *mut AVFrame,
}

// SAFETY: AVFrame is thread-safe. Access through &self/&mut self ensures
// Rust borrow check prevents data races.
unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}

impl Frame {
    /// Allocate an empty frame.
    pub fn new() -> Result<Self> {
        // SAFETY: av_frame_alloc returns a heap-allocated AVFrame or NULL.
        let ptr = unsafe { av_frame_alloc() };
        if ptr.is_null() {
            anyhow::bail!("Failed to allocate AVFrame");
        }
        Ok(Self { ptr })
    }

    /// Allocate a frame with the given pixel format and dimensions.
    ///
    /// Internally calls `av_frame_get_buffer` to allocate the data planes.
    pub fn new_with_format(pix_fmt: i32, width: i32, height: i32) -> Result<Self> {
        let frame = Self::new()?;
        // SAFETY: frame.ptr is a valid, freshly-allocated AVFrame.
        unsafe {
            (*frame.ptr).format = pix_fmt;
            (*frame.ptr).width = width;
            (*frame.ptr).height = height;
        }
        // SAFETY: format, width, and height are set. av_frame_get_buffer
        // allocates the internal data planes.
        let ret = unsafe { av_frame_get_buffer(frame.ptr, 0) };
        if ret < 0 {
            anyhow::bail!(
                "Failed to allocate frame buffer: {}",
                crate::ffmpeg_error(ret)
            );
        }
        Ok(frame)
    }

    // ── Pointer access ──

    pub fn as_ptr(&self) -> *const AVFrame {
        self.ptr
    }

    pub fn as_mut_ptr(&mut self) -> *mut AVFrame {
        self.ptr
    }

    // ── Field accessors ──

    pub fn width(&self) -> i32 {
        // SAFETY: reading field from valid AVFrame pointer.
        unsafe { (*self.ptr).width }
    }

    pub fn height(&self) -> i32 {
        // SAFETY: reading field from valid AVFrame pointer.
        unsafe { (*self.ptr).height }
    }

    pub fn format(&self) -> i32 {
        // SAFETY: reading field from valid AVFrame pointer.
        unsafe { (*self.ptr).format }
    }

    pub fn pts(&self) -> Option<i64> {
        // SAFETY: reading pts from valid AVFrame pointer.
        unsafe {
            let pts = (*self.ptr).pts;
            if pts != AV_NOPTS_VALUE {
                Some(pts)
            } else {
                None
            }
        }
    }

    pub fn set_pts(&mut self, pts: i64) {
        // SAFETY: writing pts on valid mutable AVFrame pointer.
        unsafe {
            (*self.ptr).pts = pts;
        }
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        // SAFETY: av_frame_free frees the frame and its internal buffers.
        // It also NULLs the pointer.
        unsafe {
            av_frame_free(&mut self.ptr);
        }
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use ffmpeg_sys_next::AVPixelFormat;

    #[test]
    fn alloc_and_drop() {
        let f = Frame::new().expect("alloc should succeed");
        assert_eq!(f.width(), 0);
        assert_eq!(f.height(), 0);
        assert_eq!(f.pts(), None);
    }

    #[test]
    fn new_with_format() {
        let f = Frame::new_with_format(AVPixelFormat::AV_PIX_FMT_YUV420P as i32, 1920, 1080)
            .expect("new_with_format should succeed");
        assert_eq!(f.width(), 1920);
        assert_eq!(f.height(), 1080);
        assert_eq!(f.format(), AVPixelFormat::AV_PIX_FMT_YUV420P as i32);
    }

    #[test]
    fn set_and_get_pts() {
        let mut f = Frame::new().unwrap();
        f.set_pts(12345);
        assert_eq!(f.pts(), Some(12345));
    }
}
