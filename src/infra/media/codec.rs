use std::ffi::CStr;

use anyhow::Result;
use ffmpeg_sys_next::*;
use rml_rtmp::sessions::StreamMetadata;

use crate::infra::media::ffmpeg_error;

#[allow(unused)]
pub trait CodecParamsPtrTrait {
    unsafe fn ptr(&self) -> *const AVCodecParameters;

    fn available(&self) -> bool {
        unsafe { !self.ptr().is_null() }
    }
}

pub trait CodecParamsMutPtrTrait: CodecParamsPtrTrait {
    unsafe fn mut_ptr(&mut self) -> *mut AVCodecParameters;
}

#[allow(unused)]
pub trait CodecParamsDescriptorTrait {
    fn codec_type(&self) -> AVMediaType;

    fn codec_id(&self) -> AVCodecID;

    fn profile_name(&self) -> String;

    fn codec_name(&self) -> String;
}

impl<T> CodecParamsDescriptorTrait for T
where
    T: CodecParamsPtrTrait + ?Sized,
{
    fn codec_type(&self) -> AVMediaType {
        unsafe { (*self.ptr()).codec_type }
    }

    fn codec_id(&self) -> AVCodecID {
        unsafe { (*self.ptr()).codec_id }
    }

    fn profile_name(&self) -> String {
        let name = unsafe { avcodec_profile_name(self.codec_id(), (*self.ptr()).profile) };
        unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .to_string()
    }

    fn codec_name(&self) -> String {
        let name = unsafe { avcodec_get_name(self.codec_id()) };
        unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .to_string()
    }
}

impl CodecParamsPtrTrait for *mut AVCodecParameters {
    unsafe fn ptr(&self) -> *const AVCodecParameters {
        *self
    }
}

impl CodecParamsMutPtrTrait for *mut AVCodecParameters {
    unsafe fn mut_ptr(&mut self) -> *mut AVCodecParameters {
        *self
    }
}

impl CodecParamsPtrTrait for *const AVCodecParameters {
    unsafe fn ptr(&self) -> *const AVCodecParameters {
        *self
    }
}

impl CodecParamsPtrTrait for AVCodecParameters {
    unsafe fn ptr(&self) -> *const AVCodecParameters {
        self
    }
}

impl CodecParamsMutPtrTrait for AVCodecParameters {
    unsafe fn mut_ptr(&mut self) -> *mut AVCodecParameters {
        self
    }
}

pub struct OwnedCodecParams {
    ptr: *const AVCodecParameters,
}

unsafe impl Send for OwnedCodecParams {}
unsafe impl Sync for OwnedCodecParams {}

impl OwnedCodecParams {
    pub fn copy_from(params: &dyn CodecParamsPtrTrait) -> Result<Self> {
        if unsafe { params.ptr() }.is_null() {
            anyhow::bail!("Codec parameters are not available for copying");
        }

        let ptr = alloc_codec_params()?;

        let ret = unsafe { avcodec_parameters_copy(ptr, params.ptr()) };
        if ret < 0 {
            anyhow::bail!("Failed to copy codec parameters: {}", ffmpeg_error(ret))
        }

        Ok(Self { ptr })
    }

    pub fn create_dummy_video(metadata: &StreamMetadata) -> Result<Self> {
        let codec_id = metadata
            .video_codec_id
            .ok_or(anyhow::anyhow!("Video codec ID is missing"))
            .and_then(map_rtmp_video_codec_id)?;
        // let bitrate_kbps = metadata
        //     .video_bitrate_kbps
        //     .ok_or(anyhow::anyhow!("Video bitrate is missing"))?;
        let width = metadata
            .video_width
            .ok_or(anyhow::anyhow!("Video width is missing"))?;
        let height = metadata
            .video_height
            .ok_or(anyhow::anyhow!("Video height is missing"))?;
        let frame_rate = metadata
            .video_frame_rate
            .ok_or(anyhow::anyhow!("Video frame rate is missing"))?;

        let codec = find_decoder(codec_id)?;
        let mut codec_ctx = alloc_codec_context(codec)?;

        unsafe {
            (*codec_ctx).framerate = AVRational {
                num: (frame_rate * 1000.0) as i32,
                den: 1000,
            };

            (*codec_ctx).width = width as i32;
            (*codec_ctx).height = height as i32;
        }

        let mut codec_params = match alloc_codec_params() {
            Ok(params) => params,
            Err(e) => {
                unsafe { avcodec_free_context(&mut codec_ctx) };
                return Err(e);
            }
        };

        let ret = unsafe { avcodec_parameters_from_context(codec_params, codec_ctx) };
        if ret < 0 {
            unsafe { avcodec_parameters_free(&mut codec_params) };
            unsafe { avcodec_free_context(&mut codec_ctx) };

            let err_msg = ffmpeg_error(ret);
            anyhow::bail!("Failed to copy codec parameters: {}", err_msg);
        }

        unsafe { avcodec_free_context(&mut codec_ctx) };

        Ok(Self { ptr: codec_params })
    }

    pub fn create_dummy_audio(metadata: &StreamMetadata) -> Result<Self> {
        let codec_id = metadata
            .audio_codec_id
            .ok_or(anyhow::anyhow!("Audio codec ID is missing"))
            .and_then(map_rtmp_audio_codec_id)?;
        // let bitrate_kbps = metadata
        //     .audio_bitrate_kbps
        //     .ok_or(anyhow::anyhow!("Audio bitrate is missing"))?;
        let sample_rate = metadata
            .audio_sample_rate
            .ok_or(anyhow::anyhow!("Audio sample rate is missing"))?;
        let channels = metadata
            .audio_channels
            .or_else(|| metadata.audio_is_stereo.map(|s| if s { 2 } else { 1 }))
            .unwrap_or(2);

        let codec = find_decoder(codec_id)?;
        let mut codec_ctx = alloc_codec_context(codec)?;

        unsafe {
            (*codec_ctx).sample_rate = sample_rate as i32;
            av_channel_layout_default(&mut (*codec_ctx).ch_layout, channels as i32);
        }

        let mut codec_params = match alloc_codec_params() {
            Ok(params) => params,
            Err(e) => {
                unsafe { avcodec_free_context(&mut codec_ctx) };
                return Err(e);
            }
        };

        let ret = unsafe { avcodec_parameters_from_context(codec_params, codec_ctx) };
        if ret < 0 {
            unsafe { avcodec_parameters_free(&mut codec_params) };
            unsafe { avcodec_free_context(&mut codec_ctx) };

            let err_msg = ffmpeg_error(ret);
            anyhow::bail!("Failed to copy codec parameters: {}", err_msg);
        }

        unsafe { avcodec_free_context(&mut codec_ctx) };

        Ok(Self { ptr: codec_params })
    }
}

fn map_rtmp_video_codec_id(codec_id: u32) -> Result<AVCodecID> {
    // RTMP metadata uses FLV codec ids; convert them to FFmpeg codec ids.
    let mapped = match codec_id {
        // FLV AVC
        7 => AVCodecID::AV_CODEC_ID_H264,
        // FLV HEVC (non-standard but seen in some implementations)
        12 => AVCodecID::AV_CODEC_ID_HEVC,

        // Already mapped as an AVCodecID by upstream producer.
        x if x == AVCodecID::AV_CODEC_ID_H264 as u32 => AVCodecID::AV_CODEC_ID_H264,
        x if x == AVCodecID::AV_CODEC_ID_HEVC as u32 => AVCodecID::AV_CODEC_ID_HEVC,
        x => {
            anyhow::bail!("Unsupported RTMP video codec id: {}", x);
        }
    };

    Ok(mapped)
}

fn map_rtmp_audio_codec_id(codec_id: u32) -> Result<AVCodecID> {
    // RTMP metadata uses FLV sound format ids; convert them to FFmpeg codec ids.
    let mapped = match codec_id {
        // FLV MP3
        2 | 14 => AVCodecID::AV_CODEC_ID_MP3,
        // FLV G711 A-law / mu-law
        7 => AVCodecID::AV_CODEC_ID_PCM_ALAW,
        8 => AVCodecID::AV_CODEC_ID_PCM_MULAW,
        // FLV AAC
        10 => AVCodecID::AV_CODEC_ID_AAC,

        // Already mapped as an AVCodecID by upstream producer.
        x if x == AVCodecID::AV_CODEC_ID_AAC as u32 => AVCodecID::AV_CODEC_ID_AAC,
        x if x == AVCodecID::AV_CODEC_ID_MP3 as u32 => AVCodecID::AV_CODEC_ID_MP3,
        x if x == AVCodecID::AV_CODEC_ID_PCM_ALAW as u32 => AVCodecID::AV_CODEC_ID_PCM_ALAW,
        x if x == AVCodecID::AV_CODEC_ID_PCM_MULAW as u32 => AVCodecID::AV_CODEC_ID_PCM_MULAW,
        x => {
            anyhow::bail!("Unsupported RTMP audio codec id: {}", x);
        }
    };

    Ok(mapped)
}

impl CodecParamsPtrTrait for OwnedCodecParams {
    unsafe fn ptr(&self) -> *const AVCodecParameters {
        self.ptr
    }
}

impl Drop for OwnedCodecParams {
    fn drop(&mut self) {
        unsafe { avcodec_parameters_free(&mut (self.ptr as *mut AVCodecParameters)) };
    }
}

fn find_decoder(codec_id: AVCodecID) -> Result<*const AVCodec> {
    let codec = unsafe { avcodec_find_decoder(codec_id) };
    if codec.is_null() {
        anyhow::bail!("Unsupported codec ID: {:?}", codec_id);
    }
    Ok(codec)
}

fn alloc_codec_context(codec: *const AVCodec) -> Result<*mut AVCodecContext> {
    let codec_ctx = unsafe { avcodec_alloc_context3(codec) };
    if codec_ctx.is_null() {
        anyhow::bail!("Failed to allocate AVCodecContext");
    }
    Ok(codec_ctx)
}

fn alloc_codec_params() -> Result<*mut AVCodecParameters> {
    let codec_params = unsafe { avcodec_parameters_alloc() };
    if codec_params.is_null() {
        anyhow::bail!("Failed to allocate AVCodecParameters");
    }
    Ok(codec_params)
}
