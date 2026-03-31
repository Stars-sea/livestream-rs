use std::ffi::CStr;

use anyhow::Result;
use ffmpeg_sys_next::*;

use crate::infra::media::{AudioMetadata, VideoMetadata, ffmpeg_error};

pub trait CodecParamsTrait {
    unsafe fn ptr(&self) -> *const AVCodecParameters;

    fn available(&self) -> bool {
        !unsafe { self.ptr() }.is_null()
    }

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

impl CodecParamsTrait for *mut AVCodecParameters {
    unsafe fn ptr(&self) -> *const AVCodecParameters {
        *self
    }
}

impl CodecParamsTrait for *const AVCodecParameters {
    unsafe fn ptr(&self) -> *const AVCodecParameters {
        *self
    }
}

pub struct OwnedCodecParams {
    ptr: *const AVCodecParameters,
}

impl OwnedCodecParams {
    pub fn new(ptr: *const AVCodecParameters) -> Self {
        Self { ptr }
    }
}

impl CodecParamsTrait for OwnedCodecParams {
    unsafe fn ptr(&self) -> *const AVCodecParameters {
        self.ptr
    }
}

pub struct DummyVideoCodecParams {
    pub metadata: Box<dyn VideoMetadata>,

    ptr: *const AVCodecParameters,
}

impl DummyVideoCodecParams {
    pub fn create(metadata: Box<dyn VideoMetadata>) -> Result<Self> {
        let codec = find_decoder(metadata.codec_id())?;
        let mut codec_ctx = alloc_codec_context(codec)?;

        unsafe {
            (*codec_ctx).framerate = AVRational {
                num: (metadata.frame_rate() * 1000.0) as i32,
                den: 1000,
            };

            (*codec_ctx).width = metadata.width() as i32;
            (*codec_ctx).height = metadata.height() as i32;
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

        Ok(Self {
            metadata,
            ptr: codec_params,
        })
    }
}

impl CodecParamsTrait for DummyVideoCodecParams {
    unsafe fn ptr(&self) -> *const AVCodecParameters {
        self.ptr
    }
}

impl Drop for DummyVideoCodecParams {
    fn drop(&mut self) {
        unsafe { avcodec_parameters_free(&mut (self.ptr as *mut AVCodecParameters)) };
    }
}

pub struct DummyAudioCodecParams {
    pub metadata: Box<dyn AudioMetadata>,

    ptr: *const AVCodecParameters,
}

impl DummyAudioCodecParams {
    pub fn create(metadata: Box<dyn AudioMetadata>) -> Result<Self> {
        let codec = find_decoder(metadata.codec_id())?;
        let mut codec_ctx = alloc_codec_context(codec)?;

        unsafe {
            (*codec_ctx).sample_rate = metadata.sample_rate() as i32;
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

        Ok(Self {
            metadata,
            ptr: codec_params,
        })
    }
}

impl CodecParamsTrait for DummyAudioCodecParams {
    unsafe fn ptr(&self) -> *const AVCodecParameters {
        self.ptr
    }
}

impl Drop for DummyAudioCodecParams {
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
