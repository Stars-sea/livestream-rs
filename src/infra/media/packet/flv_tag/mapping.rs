use anyhow::Result;
use ffmpeg_sys_next::{AVMediaType, AVRational};

use crate::infra::media::codec::CodecParamsDescriptorTrait;
use crate::infra::media::stream::StreamCollection;

pub(super) struct FlvStreamMapping {
    pub(super) audio_stream_idx: usize,
    pub(super) video_stream_idx: usize,
    pub(super) audio_time_base: AVRational,
    pub(super) video_time_base: AVRational,
}

impl FlvStreamMapping {
    pub(super) fn from_streams(streams: &dyn StreamCollection) -> Result<Self> {
        let mut audio_stream_idx: Option<usize> = None;
        let mut video_stream_idx: Option<usize> = None;
        let mut audio_time_base: Option<AVRational> = None;
        let mut video_time_base: Option<AVRational> = None;

        for stream in streams {
            let codec_params = stream.codec_params_ptr();
            match codec_params.codec_type() {
                AVMediaType::AVMEDIA_TYPE_AUDIO if audio_stream_idx.is_none() => {
                    audio_stream_idx = Some(stream.index());
                    audio_time_base = Some(stream.time_base());
                }
                AVMediaType::AVMEDIA_TYPE_VIDEO if video_stream_idx.is_none() => {
                    video_stream_idx = Some(stream.index());
                    video_time_base = Some(stream.time_base());
                }
                _ => {}
            }
        }

        let audio_stream_idx =
            audio_stream_idx.ok_or_else(|| anyhow::anyhow!("Audio stream index not found"))?;
        let video_stream_idx =
            video_stream_idx.ok_or_else(|| anyhow::anyhow!("Video stream index not found"))?;
        let audio_time_base =
            audio_time_base.ok_or_else(|| anyhow::anyhow!("Audio stream time base not found"))?;
        let video_time_base =
            video_time_base.ok_or_else(|| anyhow::anyhow!("Video stream time base not found"))?;

        Ok(Self {
            audio_stream_idx,
            video_stream_idx,
            audio_time_base,
            video_time_base,
        })
    }
}
