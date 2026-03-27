//! FFmpeg stream wrapper with safe access to stream properties.

use ffmpeg_sys_next::AVMediaType::AVMEDIA_TYPE_VIDEO;
use ffmpeg_sys_next::*;

/// FFmpeg's AVStream accessor methods.
pub trait StreamTrait {
    unsafe fn ptr(&self) -> *mut AVStream;

    /// Returns the time base for this stream.
    fn time_base(&self) -> AVRational {
        unsafe { (*self.ptr()).time_base }
    }

    /// Returns the index of this stream within the parent AVFormatContext.
    fn index(&self) -> usize {
        unsafe { (*self.ptr()).index as usize }
    }

    /// Returns the codec parameters for this stream.
    fn codec_params(&self) -> *mut AVCodecParameters {
        unsafe { (*self.ptr()).codecpar }
    }

    /// Checks if this stream contains video data.
    fn is_video_stream(&self) -> bool {
        unsafe { (*self.codec_params()).codec_type == AVMEDIA_TYPE_VIDEO }
    }
}

impl StreamTrait for *mut AVStream {
    unsafe fn ptr(&self) -> *mut AVStream {
        *self
    }
}
