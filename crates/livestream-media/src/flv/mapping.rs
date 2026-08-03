//! FLV stream mapping — maps FLV tag types to stream indices and timebases.

use ffmpeg_sys_next::{AVMediaType, AVRational};

use crate::codec::CodecParamsDescriptorTrait;
use crate::stream::StreamCollection;

/// Maps FLV audio/video streams to their indices and timebases.
///
/// Either audio or video (or both) may be absent — e.g., audio-only streams
/// have no video mapping.
pub(super) struct FlvStreamMapping {
    pub(super) audio: Option<(usize, AVRational)>,
    pub(super) video: Option<(usize, AVRational)>,
}

impl FlvStreamMapping {
    pub(super) fn from_streams(streams: &dyn StreamCollection) -> Self {
        let mut audio: Option<(usize, AVRational)> = None;
        let mut video: Option<(usize, AVRational)> = None;

        for stream in streams {
            let cp = stream.codec_params_ptr();
            let ct = cp.codec_type();
            if ct == AVMediaType::AVMEDIA_TYPE_AUDIO && audio.is_none() {
                audio = Some((stream.index(), stream.time_base()));
            }
            if ct == AVMediaType::AVMEDIA_TYPE_VIDEO && video.is_none() {
                video = Some((stream.index(), stream.time_base()));
            }
        }

        Self { audio, video }
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::OwnedCodecParams;
    use crate::stream::StaticStreamCollection;
    use ffmpeg_sys_next::AVCodecID;

    fn make_streams() -> StaticStreamCollection {
        let video =
            OwnedCodecParams::create_dummy_video(AVCodecID::AV_CODEC_ID_H264, 640, 360, 30.0)
                .unwrap();
        let audio =
            OwnedCodecParams::create_dummy_audio(AVCodecID::AV_CODEC_ID_AAC, 44100, 2).unwrap();
        StaticStreamCollection::from_owned_params(vec![
            (0, AVRational { num: 1, den: 90000 }, video),
            (1, AVRational { num: 1, den: 44100 }, audio),
        ])
    }

    #[test]
    fn maps_both_streams() {
        let mapping = FlvStreamMapping::from_streams(&make_streams());
        assert_eq!(mapping.audio, Some((1, AVRational { num: 1, den: 44100 })));
        assert_eq!(mapping.video, Some((0, AVRational { num: 1, den: 90000 })));
    }

    #[test]
    fn empty_collection_yields_none() {
        let streams = StaticStreamCollection::from_owned_params(vec![]);
        let mapping = FlvStreamMapping::from_streams(&streams);
        assert!(mapping.audio.is_none());
        assert!(mapping.video.is_none());
    }

    #[test]
    fn audio_only_yields_none_video() {
        let audio =
            OwnedCodecParams::create_dummy_audio(AVCodecID::AV_CODEC_ID_AAC, 44100, 2).unwrap();
        let streams = StaticStreamCollection::from_owned_params(vec![(
            1,
            AVRational { num: 1, den: 44100 },
            audio,
        )]);
        let mapping = FlvStreamMapping::from_streams(&streams);
        assert!(mapping.video.is_none());
        assert_eq!(mapping.audio, Some((1, AVRational { num: 1, den: 44100 })));
    }
}
