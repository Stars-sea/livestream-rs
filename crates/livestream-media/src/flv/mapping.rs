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
            let codec_params = stream.codec_params_ptr();
            match codec_params.codec_type() {
                AVMediaType::AVMEDIA_TYPE_AUDIO if audio.is_none() => {
                    audio = Some((stream.index(), stream.time_base()));
                }
                AVMediaType::AVMEDIA_TYPE_VIDEO if video.is_none() => {
                    video = Some((stream.index(), stream.time_base()));
                }
                _ => {}
            }
        }

        Self { audio, video }
    }
}
