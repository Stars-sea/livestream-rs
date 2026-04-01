//! FFmpeg stream wrapper with safe access to stream properties.

use ffmpeg_sys_next::*;
use rml_rtmp::sessions::StreamMetadata;

use crate::infra::media::codec::{CodecParamsTrait, OwnedCodecParams};
use crate::infra::media::context::Context;

/// FFmpeg's AVStream accessor methods.
pub trait StreamTrait {
    unsafe fn ptr(&self) -> *const AVStream;

    /// Returns the time base for this stream.
    fn time_base(&self) -> AVRational {
        unsafe { (*self.ptr()).time_base }
    }

    /// Returns the index of this stream within the parent AVFormatContext.
    fn index(&self) -> usize {
        unsafe { (*self.ptr()).index as usize }
    }

    /// Returns the codec parameters pointer for this stream.
    fn codec_params_ptr(&self) -> *const AVCodecParameters {
        unsafe { (*self.ptr()).codecpar }
    }
}

pub trait StreamCollection {
    fn stream_count(&self) -> usize;

    fn stream(&self, index: usize) -> Option<Box<dyn StreamTrait + '_>>;
}

impl StreamTrait for *mut AVStream {
    unsafe fn ptr(&self) -> *const AVStream {
        *self
    }
}

impl StreamTrait for *const AVStream {
    unsafe fn ptr(&self) -> *const AVStream {
        *self
    }
}

impl<C: Context> StreamCollection for C {
    /// Returns the number of streams in the context.
    fn stream_count(&self) -> usize {
        unsafe { (*self.ptr()).nb_streams as usize }
    }

    /// Gets a stream by index.
    ///
    /// # Arguments
    /// * `id` - Stream index (must be < nb_streams())
    ///
    /// # Returns
    /// Some(Stream) if the index is valid, None otherwise.
    fn stream(&self, index: usize) -> Option<Box<dyn StreamTrait + '_>> {
        if index < self.stream_count() {
            let ptr = unsafe { *(*self.ptr()).streams.offset(index as isize) };
            Some(Box::new(ptr))
        } else {
            None
        }
    }
}

impl StreamCollection for StreamMetadata {
    fn stream_count(&self) -> usize {
        2
    }

    fn stream(&self, index: usize) -> Option<Box<dyn StreamTrait + '_>> {
        match index {
            0 => {
                let params = OwnedCodecParams::create_dummy_video(self).ok()?;
                Some(Box::new(DummyStream::Video(params)))
            }
            1 => {
                let params = OwnedCodecParams::create_dummy_audio(self).ok()?;
                Some(Box::new(DummyStream::Audio(params)))
            }
            _ => None,
        }
    }
}

pub enum DummyStream {
    Video(OwnedCodecParams),
    Audio(OwnedCodecParams),
}

impl StreamTrait for DummyStream {
    unsafe fn ptr(&self) -> *const AVStream {
        std::ptr::null()
    }

    fn time_base(&self) -> AVRational {
        AVRational { num: 1, den: 1000 }
    }

    fn index(&self) -> usize {
        match self {
            DummyStream::Video(..) => 0,
            DummyStream::Audio(..) => 1,
        }
    }

    fn codec_params_ptr(&self) -> *const AVCodecParameters {
        match self {
            DummyStream::Video(params) => unsafe { params.ptr() },
            DummyStream::Audio(params) => unsafe { params.ptr() },
        }
    }
}
