use std::fmt::Debug;

use ffmpeg_sys_next::AVCodecID;

pub trait Metadata: Send + Sync + Debug {}

pub trait VideoMetadata: Metadata {
    fn codec_id(&self) -> AVCodecID;
    fn bitrate_kbps(&self) -> u32;

    fn width(&self) -> u32;
    fn height(&self) -> u32;

    fn frame_rate(&self) -> f32;
}

pub trait AudioMetadata: Metadata {
    fn codec_id(&self) -> AVCodecID;
    fn bitrate_kbps(&self) -> u32;

    fn sample_rate(&self) -> u32;
    fn channels(&self) -> u32;
    fn is_stereo(&self) -> bool;
}
