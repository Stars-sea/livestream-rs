//! FFmpeg stream wrapper with safe access to stream properties.

use ffmpeg_sys_next::AVMediaType::AVMEDIA_TYPE_VIDEO;
use ffmpeg_sys_next::*;

/// Wrapper around FFmpeg's AVStream with safe accessor methods.
///
/// # Safety
/// This struct maintains a raw pointer to AVStream which is managed by
/// the parent AVFormatContext. The pointer is valid as long as the parent
/// context is alive.
#[derive(Copy, Clone, Debug)]
pub struct Stream {
    stream: *mut AVStream,
}

impl Stream {
    /// Creates a new Stream wrapper from a raw AVStream pointer.
    ///
    /// # Safety
    /// The caller must ensure the pointer is valid and will remain valid
    /// for the lifetime of this Stream instance.
    pub fn new(stream: *mut AVStream) -> Self {
        Self { stream }
    }

    /// Returns the time base for this stream.
    ///
    /// # Safety
    /// Accesses the raw pointer. Safe because Stream is only created
    /// from valid AVStream pointers managed by Context.
    pub fn time_base(&self) -> AVRational {
        unsafe { (*self.stream).time_base }
    }

    /// Returns the time base as a floating point value.
    pub fn time_base_f64(&self) -> f64 {
        unsafe { av_q2d(self.time_base()) }
    }

    /// Returns the codec parameters for this stream.
    ///
    /// # Safety
    /// Returns a raw pointer that is valid as long as the parent
    /// AVFormatContext is alive.
    pub fn codec_params(&self) -> *mut AVCodecParameters {
        unsafe { (*self.stream).codecpar }
    }

    /// Checks if this stream contains video data.
    pub fn is_video_stream(&self) -> bool {
        unsafe { (*self.codec_params()).codec_type == AVMEDIA_TYPE_VIDEO }
    }
}
