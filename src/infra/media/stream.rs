//! FFmpeg stream wrapper with safe access to stream properties.

use anyhow::Result;
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

    // We return boxed trait objects to keep callers decoupled from concrete stream storage.
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

// Owned snapshot for one stream. This is detached from AVFormatContext and can cross threads.
pub struct StaticStream {
    index: usize,
    time_base: AVRational,
    codec_params: OwnedCodecParams,
}

// Borrowed view wrapper used to return Box<dyn StreamTrait + '_> without cloning codec params.
struct StaticStreamRef<'a>(&'a StaticStream);

impl StreamTrait for StaticStreamRef<'_> {
    unsafe fn ptr(&self) -> *const AVStream {
        std::ptr::null()
    }

    fn time_base(&self) -> AVRational {
        self.0.time_base
    }

    fn index(&self) -> usize {
        self.0.index
    }

    fn codec_params_ptr(&self) -> *const AVCodecParameters {
        unsafe { self.0.codec_params.ptr() }
    }
}

// Snapshot collection used when we must publish stream metadata outside InputContext lifetime.
pub struct StaticStreamCollection {
    streams: Vec<StaticStream>,
}

impl StaticStreamCollection {
    // Deep-copy stream descriptors (codec params + time base + index) from any stream source.
    pub fn from_streams(streams: &dyn StreamCollection) -> Result<Self> {
        let mut static_streams = Vec::with_capacity(streams.stream_count());

        for i in 0..streams.stream_count() {
            let stream = streams
                .stream(i)
                .ok_or_else(|| anyhow::anyhow!("Stream {} not found", i))?;
            let codec_params = OwnedCodecParams::copy_from(&stream.codec_params_ptr())?;

            static_streams.push(StaticStream {
                index: stream.index(),
                time_base: stream.time_base(),
                codec_params,
            });
        }

        Ok(Self {
            streams: static_streams,
        })
    }
}

impl StreamCollection for StaticStreamCollection {
    fn stream_count(&self) -> usize {
        self.streams.len()
    }

    fn stream(&self, index: usize) -> Option<Box<dyn StreamTrait + '_>> {
        // Return a borrowed trait-object view to avoid copying codec params per call.
        self.streams
            .get(index)
            .map(|s| Box::new(StaticStreamRef(s)) as Box<dyn StreamTrait + '_>)
    }
}
