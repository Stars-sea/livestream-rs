use ffmpeg_sys_next::AVCodecID;
use rml_rtmp::sessions::StreamMetadata;

use crate::infra::media::{AudioMetadata, Metadata, VideoMetadata};

impl Metadata for StreamMetadata {}

impl VideoMetadata for StreamMetadata {
    fn codec_id(&self) -> AVCodecID {
        self.video_codec_id
            .map(|id| unsafe { std::mem::transmute(id) })
            .unwrap_or(AVCodecID::AV_CODEC_ID_NONE)
    }

    fn bitrate_kbps(&self) -> u32 {
        self.video_bitrate_kbps.unwrap_or(0)
    }

    fn width(&self) -> u32 {
        self.video_width.unwrap_or(0)
    }

    fn height(&self) -> u32 {
        self.video_height.unwrap_or(0)
    }

    fn frame_rate(&self) -> f32 {
        self.video_frame_rate.unwrap_or(0.0)
    }
}

impl AudioMetadata for StreamMetadata {
    fn codec_id(&self) -> AVCodecID {
        self.audio_codec_id
            .map(|id| unsafe { std::mem::transmute(id) })
            .unwrap_or(AVCodecID::AV_CODEC_ID_NONE)
    }

    fn bitrate_kbps(&self) -> u32 {
        self.audio_bitrate_kbps.unwrap_or(0)
    }

    fn sample_rate(&self) -> u32 {
        self.audio_sample_rate.unwrap_or(0)
    }

    fn channels(&self) -> u32 {
        self.audio_channels.unwrap_or(0)
    }

    fn is_stereo(&self) -> bool {
        self.audio_is_stereo.unwrap_or(false)
    }
}
