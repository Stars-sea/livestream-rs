//! FFmpeg stream wrapper with safe access to stream properties.

use anyhow::Result;
use ffmpeg_sys_next::*;
use rml_rtmp::sessions::StreamMetadata;

use crate::infra::media::codec::{CodecParamsPtrTrait, OwnedCodecParams};
use crate::infra::media::context::Context;

/// Trait for types that can provide a real FFmpeg AVStream pointer.
pub trait StreamPtrTrait {
    unsafe fn ptr(&self) -> *const AVStream;
}

/// Metadata access trait for stream descriptors used across the pipeline.
///
/// This trait is pointer-free for callers. Implementors may compute values from
/// owned snapshots or from a real AVStream pointer.
pub trait StreamDescriptorTrait {
    /// Returns the time base for this stream.
    fn time_base(&self) -> AVRational;

    /// Returns the index of this stream within the parent stream collection.
    fn index(&self) -> usize;

    /// Returns the codec parameters pointer for this stream.
    fn codec_params_ptr(&self) -> *const AVCodecParameters;
}

impl<T> StreamDescriptorTrait for T
where
    T: StreamPtrTrait + ?Sized,
{
    fn time_base(&self) -> AVRational {
        unsafe { (*self.ptr()).time_base }
    }

    fn index(&self) -> usize {
        unsafe { (*self.ptr()).index as usize }
    }

    fn codec_params_ptr(&self) -> *const AVCodecParameters {
        unsafe { (*self.ptr()).codecpar }
    }
}

pub trait StreamCollection {
    fn stream_count(&self) -> usize;

    // We return boxed trait objects to keep callers decoupled from concrete stream storage.
    fn stream(&self, index: usize) -> Option<Box<dyn StreamDescriptorTrait + '_>>;

    /// Fast path to retrieve the time base for a specific stream index without allocating.
    fn time_base(&self, index: usize) -> Option<AVRational> {
        self.stream(index).map(|s| s.time_base())
    }
}

pub struct StreamCollectionIter<'a, S: StreamCollection + ?Sized> {
    streams: &'a S,
    index: usize,
}

impl<'a, S: StreamCollection + ?Sized> Iterator for StreamCollectionIter<'a, S> {
    type Item = Box<dyn StreamDescriptorTrait + 'a>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.index < self.streams.stream_count() {
            let idx = self.index;
            self.index += 1;

            if let Some(stream) = self.streams.stream(idx) {
                return Some(stream);
            }
        }

        None
    }
}

pub fn iter_streams(
    streams: &dyn StreamCollection,
) -> StreamCollectionIter<'_, dyn StreamCollection + '_> {
    StreamCollectionIter { streams, index: 0 }
}

impl<'a> IntoIterator for &'a dyn StreamCollection {
    type Item = Box<dyn StreamDescriptorTrait + 'a>;
    type IntoIter = StreamCollectionIter<'a, dyn StreamCollection + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        iter_streams(self)
    }
}

impl StreamPtrTrait for *mut AVStream {
    unsafe fn ptr(&self) -> *const AVStream {
        *self
    }
}

impl StreamPtrTrait for *const AVStream {
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
    fn stream(&self, index: usize) -> Option<Box<dyn StreamDescriptorTrait + '_>> {
        if index < self.stream_count() {
            let ptr = unsafe { *(*self.ptr()).streams.offset(index as isize) };
            Some(Box::new(ptr))
        } else {
            None
        }
    }

    fn time_base(&self, index: usize) -> Option<AVRational> {
        if index < self.stream_count() {
            let ptr = unsafe { *(*self.ptr()).streams.offset(index as isize) };
            Some(unsafe { (*ptr).time_base })
        } else {
            None
        }
    }
}

impl StreamCollection for StreamMetadata {
    fn stream_count(&self) -> usize {
        2
    }

    fn stream(&self, index: usize) -> Option<Box<dyn StreamDescriptorTrait + '_>> {
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

    fn time_base(&self, index: usize) -> Option<AVRational> {
        if index < 2 {
            Some(AVRational { num: 1, den: 1000 })
        } else {
            None
        }
    }
}

pub enum DummyStream {
    Video(OwnedCodecParams),
    Audio(OwnedCodecParams),
}

impl StreamDescriptorTrait for DummyStream {
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

// Borrowed view wrapper used to return Box<dyn StreamDescriptorTrait + '_> without cloning codec params.
struct StaticStreamRef<'a>(&'a StaticStream);

impl StreamDescriptorTrait for StaticStreamRef<'_> {
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

    fn stream(&self, index: usize) -> Option<Box<dyn StreamDescriptorTrait + '_>> {
        // Return a borrowed trait-object view to avoid copying codec params per call.
        self.streams
            .get(index)
            .map(|s| Box::new(StaticStreamRef(s)) as Box<dyn StreamDescriptorTrait + '_>)
    }

    fn time_base(&self, index: usize) -> Option<AVRational> {
        self.streams.get(index).map(|s| s.time_base)
    }
}
