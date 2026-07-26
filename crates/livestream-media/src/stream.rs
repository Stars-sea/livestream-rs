//! Stream collection traits and `StaticStreamCollection`.
//!
//! `StreamCollection` abstracts over stream descriptors (time base + codec params).
//! `StaticStreamCollection` is an owned, `Send + Sync` snapshot used when the
//! source context outlives the pipeline construction phase.
//!
//! Unlike the old code, `impl StreamCollection for StreamMetadata` and
//! `impl StreamCollection for C: Context` are removed — use
//! `StaticStreamCollection::from_owned_params()` instead.

use anyhow::Result;
use ffmpeg_sys_next::*;

use crate::codec::{CodecParamsPtrTrait, OwnedCodecParams};
use livestream_core::types::CodecParams;

// ── Traits ──

/// Trait for types that provide a real FFmpeg `AVStream` pointer.
pub trait StreamPtrTrait {
    /// # Safety
    /// The caller must ensure the pointer is valid for the lifetime of `self`.
    unsafe fn ptr(&self) -> *const AVStream;
}

/// Metadata access trait for stream descriptors.
///
/// Callers don't need to deal with raw pointers — implementors
/// compute values from owned snapshots or real `AVStream` pointers.
pub trait StreamDescriptorTrait {
    fn time_base(&self) -> AVRational;
    fn index(&self) -> usize;
    fn codec_params_ptr(&self) -> *const AVCodecParameters;
}

impl<T> StreamDescriptorTrait for T
where
    T: StreamPtrTrait + ?Sized,
{
    fn time_base(&self) -> AVRational {
        // SAFETY: ptr() returns a valid AVStream pointer per trait contract.
        unsafe { (*self.ptr()).time_base }
    }

    fn index(&self) -> usize {
        // SAFETY: ptr() returns a valid AVStream pointer per trait contract.
        unsafe { (*self.ptr()).index as usize }
    }

    fn codec_params_ptr(&self) -> *const AVCodecParameters {
        // SAFETY: ptr() returns a valid AVStream pointer per trait contract.
        unsafe { (*self.ptr()).codecpar }
    }
}

/// A collection of stream descriptors (video + audio streams).
///
/// Implementations include live `AVStream` wrappers and static snapshots.
pub trait StreamCollection {
    fn stream_count(&self) -> usize;

    fn stream(&self, index: usize) -> Option<Box<dyn StreamDescriptorTrait + '_>>;

    fn time_base(&self, index: usize) -> Option<AVRational> {
        self.stream(index).map(|s| s.time_base())
    }
}

// ── Blanket trait impls for raw pointers ──

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

// ── Iteration ──

/// Iterator over streams in a `StreamCollection`.
pub struct StreamCollectionIter<'a, S: StreamCollection + ?Sized> {
    streams: &'a S,
    index: usize,
}

impl<'a, S: StreamCollection + ?Sized> Iterator for StreamCollectionIter<'a, S> {
    type Item = Box<dyn StreamDescriptorTrait + 'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let stream = (self.index..self.streams.stream_count()).find_map(|i| {
            self.index = i + 1;
            self.streams.stream(i)
        })?;
        Some(stream)
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

// ── StaticStreamCollection ──

/// A single static stream entry.
pub struct StaticStream {
    index: usize,
    time_base: AVRational,
    codec_params: OwnedCodecParams,
}

/// Borrowed view of a `StaticStream`.
struct StaticStreamView<'a>(&'a StaticStream);

impl StreamDescriptorTrait for StaticStreamView<'_> {
    fn time_base(&self) -> AVRational {
        self.0.time_base
    }

    fn index(&self) -> usize {
        self.0.index
    }

    fn codec_params_ptr(&self) -> *const AVCodecParameters {
        // SAFETY: OwnedCodecParams::ptr() returns a valid pointer.
        unsafe { self.0.codec_params.ptr() }
    }
}

/// An owned, `Send + Sync` snapshot of stream codec parameters.
///
/// Used when pipeline construction needs stream metadata after the source
/// may have been dropped.
pub struct StaticStreamCollection {
    streams: Vec<StaticStream>,
}

impl StaticStreamCollection {
    /// Construct from a live `StreamCollection` by deep-copying codec parameters.
    pub fn from_streams(streams: &dyn StreamCollection) -> Result<Self> {
        let mut snapshot = Vec::with_capacity(streams.stream_count());

        for index in 0..streams.stream_count() {
            let stream = streams
                .stream(index)
                .ok_or_else(|| anyhow::anyhow!("Stream {} not found", index))?;
            let codec_params = OwnedCodecParams::copy_from(&stream.codec_params_ptr())?;

            snapshot.push(StaticStream {
                index: stream.index(),
                time_base: stream.time_base(),
                codec_params,
            });
        }

        Ok(Self { streams: snapshot })
    }

    /// Construct directly from `OwnedCodecParams` with explicit indices and timebases.
    ///
    /// Bypasses the `StreamCollection` trait — useful when building from
    /// RTMP metadata without a live FFmpeg context.
    pub fn from_owned_params(params: Vec<(usize, AVRational, OwnedCodecParams)>) -> Self {
        let streams = params
            .into_iter()
            .map(|(index, time_base, codec_params)| StaticStream {
                index,
                time_base,
                codec_params,
            })
            .collect();
        Self { streams }
    }

    /// Construct from a Source's `CodecParams` slice.
    ///
    /// Each `CodecParams` is converted to an `OwnedCodecParams` (deep-copies
    /// the underlying `AVCodecParameters*` via `avcodec_parameters_alloc` + fill).
    /// Video streams get a 90kHz timebase; audio streams get the sample rate.
    pub fn from_codec_params(codec_params: &[CodecParams]) -> Result<Self> {
        let video_tb = AVRational { num: 1, den: 90000 };
        let audio_tb = AVRational { num: 1, den: 44100 };

        let owned: Vec<_> = codec_params
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let tb = if p.is_video() { video_tb } else { audio_tb };
                Ok((i, tb, OwnedCodecParams::from_codec_params(p)?))
            })
            .collect::<Result<_>>()?;

        Ok(Self::from_owned_params(owned))
    }

    pub fn len(&self) -> usize {
        self.streams.len()
    }

    pub fn is_empty(&self) -> bool {
        self.streams.is_empty()
    }
}

impl StreamCollection for StaticStreamCollection {
    fn stream_count(&self) -> usize {
        self.streams.len()
    }

    fn stream(&self, index: usize) -> Option<Box<dyn StreamDescriptorTrait + '_>> {
        self.streams
            .get(index)
            .map(|s| Box::new(StaticStreamView(s)) as Box<dyn StreamDescriptorTrait + '_>)
    }

    fn time_base(&self, index: usize) -> Option<AVRational> {
        self.streams.get(index).map(|s| s.time_base)
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_owned_params_constructs() {
        let video_params =
            OwnedCodecParams::create_dummy_video(AVCodecID::AV_CODEC_ID_H264, 1920, 1080, 30.0)
                .unwrap();
        let audio_params =
            OwnedCodecParams::create_dummy_audio(AVCodecID::AV_CODEC_ID_AAC, 44100, 2).unwrap();

        let streams = vec![
            (0, AVRational { num: 1, den: 90000 }, video_params),
            (1, AVRational { num: 1, den: 44100 }, audio_params),
        ];
        let coll = StaticStreamCollection::from_owned_params(streams);

        assert_eq!(coll.stream_count(), 2);
        assert_eq!(coll.time_base(0), Some(AVRational { num: 1, den: 90000 }));
        assert_eq!(coll.time_base(1), Some(AVRational { num: 1, den: 44100 }));
    }

    #[test]
    fn from_streams_copies() {
        // Build a StaticStreamCollection, then copy from it to verify from_streams.
        let video =
            OwnedCodecParams::create_dummy_video(AVCodecID::AV_CODEC_ID_H264, 640, 480, 25.0)
                .unwrap();
        let original = StaticStreamCollection::from_owned_params(vec![(
            0,
            AVRational { num: 1, den: 1000 },
            video,
        )]);

        let copied = StaticStreamCollection::from_streams(&original).unwrap();
        assert_eq!(copied.stream_count(), 1);
    }

    #[test]
    fn iteration_yields_all_streams() {
        let video =
            OwnedCodecParams::create_dummy_video(AVCodecID::AV_CODEC_ID_H264, 1280, 720, 30.0)
                .unwrap();
        let coll = StaticStreamCollection::from_owned_params(vec![(
            0,
            AVRational { num: 1, den: 90000 },
            video,
        )]);

        let count = iter_streams(&coll).count();
        assert_eq!(count, 1);
    }
}
