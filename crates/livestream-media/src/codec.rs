//! FFmpeg codec parameter traits and `OwnedCodecParams` RAII wrapper.
//!
//! Provides safe abstractions over `AVCodecParameters` and codec id mapping
//! helpers for FLV→FFmpeg conversion.

use anyhow::Result;
use ffmpeg_sys_next::*;
use livestream_codec::Codec;

use std::ffi::CStr;

use crate::ffmpeg_error;

// ── Traits ──

/// Trait for types that provide a read-only `AVCodecParameters` pointer.
pub trait CodecParamsPtrTrait {
    /// # Safety
    /// The returned pointer must be valid for the lifetime of `self`.
    unsafe fn ptr(&self) -> *const AVCodecParameters;

    fn available(&self) -> bool {
        // SAFETY: ptr() is safe to call — caller's responsibility per trait contract.
        unsafe { !self.ptr().is_null() }
    }
}

/// Trait for types that provide a mutable `AVCodecParameters` pointer.
pub trait CodecParamsMutPtrTrait: CodecParamsPtrTrait {
    /// # Safety
    /// The returned pointer must be valid for the lifetime of `self`.
    unsafe fn mut_ptr(&mut self) -> *mut AVCodecParameters;
}

/// Descriptor trait providing access to common `AVCodecParameters` fields.
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
        // SAFETY: ptr() returns a valid pointer per trait contract.
        unsafe { (*self.ptr()).codec_type }
    }

    fn codec_id(&self) -> AVCodecID {
        // SAFETY: ptr() returns a valid pointer per trait contract.
        unsafe { (*self.ptr()).codec_id }
    }

    fn profile_name(&self) -> String {
        // SAFETY: avcodec_profile_name returns a static C string or NULL.
        let name = unsafe { avcodec_profile_name(self.codec_id(), (*self.ptr()).profile) };
        if name.is_null() {
            return "unknown".to_string();
        }
        // SAFETY: avcodec_profile_name returns a null-terminated C string.
        unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .to_string()
    }

    fn codec_name(&self) -> String {
        // SAFETY: avcodec_get_name returns a static C string or NULL.
        let name = unsafe { avcodec_get_name(self.codec_id()) };
        if name.is_null() {
            return "unknown".to_string();
        }
        // SAFETY: avcodec_get_name returns a null-terminated C string.
        unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .to_string()
    }
}

// ── Trait impls for FFmpeg raw types ──

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

// ── OwnedCodecParams ──

/// Owned FFmpeg `AVCodecParameters` with RAII cleanup.
///
/// Drop calls `avcodec_parameters_free`.
pub struct OwnedCodecParams {
    ptr: *const AVCodecParameters,
}

// SAFETY: AVCodecParameters is not mutated after construction (the pointer
// is treated as *const). avcodec_parameters_free is the only mutation and
// happens exclusively in Drop.
unsafe impl Send for OwnedCodecParams {}
unsafe impl Sync for OwnedCodecParams {}

impl OwnedCodecParams {
    /// Deep-copy codec parameters from a trait object.
    pub fn copy_from(params: &dyn CodecParamsPtrTrait) -> Result<Self> {
        // SAFETY: ptr() returns a valid pointer per trait contract.
        if unsafe { params.ptr() }.is_null() {
            anyhow::bail!("Codec parameters are not available for copying");
        }

        let mut ptr = alloc_codec_params()?;
        // SAFETY: Both pointers are valid, non-null AVCodecParameters.
        let ret = unsafe { avcodec_parameters_copy(ptr, params.ptr()) };
        if ret < 0 {
            // SAFETY: ptr was just allocated above; free it on failure.
            unsafe {
                avcodec_parameters_free(&mut ptr);
            }
            anyhow::bail!("Failed to copy codec parameters: {}", ffmpeg_error(ret));
        }

        Ok(Self { ptr })
    }

    /// Construct `OwnedCodecParams` from a pure-Rust `CodecParams`.
    ///
    /// Allocates a new `AVCodecParameters` and fills it based on the
    /// `Codec` variant and optional extradata.
    pub fn from_codec_params(params: &livestream_core::types::CodecParams) -> Result<Self> {
        let mut ptr = alloc_codec_params()?;

        // SAFETY: ptr is a freshly allocated AVCodecParameters.
        unsafe {
            match params.codec {
                Codec::H264 | Codec::H265 | Codec::Av1 => {
                    (*ptr).codec_type = AVMediaType::AVMEDIA_TYPE_VIDEO;
                }
                Codec::Aac | Codec::Opus | Codec::Mp3 => {
                    (*ptr).codec_type = AVMediaType::AVMEDIA_TYPE_AUDIO;
                    (*ptr).sample_rate = params.clock_rate as i32;
                }
            }
            (*ptr).codec_id = codec_to_av_codec_id(params.codec);
        }

        // Copy extradata if present.
        if let Some(ref extradata) = params.extradata {
            copy_extradata_to_codecpar(&mut ptr, extradata)?;
        }

        Ok(Self { ptr })
    }

    /// Create a fake/dummy video codec params from raw values.
    ///
    /// Used when RTMP metadata is the only source of codec information
    /// (no FFmpeg input context available). Creates an `AVCodecContext`,
    /// sets basic fields, and copies parameters out via
    /// `avcodec_parameters_from_context`.
    pub fn create_dummy_video(
        codec_id: AVCodecID,
        width: u32,
        height: u32,
        frame_rate: f64,
    ) -> Result<Self> {
        let codec = find_decoder(codec_id)?;
        let codec_ctx = alloc_codec_context(codec)?;

        // SAFETY: codec_ctx is a freshly allocated AVCodecContext.
        unsafe {
            (*codec_ctx).framerate = AVRational {
                num: (frame_rate * 1000.0) as i32,
                den: 1000,
            };
            (*codec_ctx).width = width as i32;
            (*codec_ctx).height = height as i32;
        }

        create_params_from_ctx(codec_ctx)
    }

    /// Create a fake/dummy audio codec params from raw values.
    pub fn create_dummy_audio(
        codec_id: AVCodecID,
        sample_rate: u32,
        channels: u32,
    ) -> Result<Self> {
        let codec = find_decoder(codec_id)?;
        let codec_ctx = alloc_codec_context(codec)?;

        // SAFETY: codec_ctx is a freshly allocated AVCodecContext.
        unsafe {
            (*codec_ctx).sample_rate = sample_rate as i32;
            av_channel_layout_default(&mut (*codec_ctx).ch_layout, channels as i32);
        }

        create_params_from_ctx(codec_ctx)
    }
}

/// Extract `AVCodecParameters` from a temporary `AVCodecContext`,
/// then free the context.
fn create_params_from_ctx(mut codec_ctx: *mut AVCodecContext) -> Result<OwnedCodecParams> {
    let mut codec_params = match alloc_codec_params() {
        Ok(params) => params,
        Err(e) => {
            // SAFETY: codec_ctx was just allocated; free on error.
            unsafe {
                avcodec_free_context(&mut codec_ctx);
            }
            return Err(e);
        }
    };

    // SAFETY: codec_params and codec_ctx are both valid.
    let ret = unsafe { avcodec_parameters_from_context(codec_params, codec_ctx) };
    // SAFETY: free the temporary codec context regardless of success/failure.
    unsafe {
        avcodec_free_context(&mut codec_ctx);
    }
    if ret < 0 {
        // SAFETY: codec_params was allocated above; free on failure.
        unsafe {
            avcodec_parameters_free(&mut codec_params);
        }
        anyhow::bail!(
            "Failed to copy codec parameters from context: {}",
            ffmpeg_error(ret)
        );
    }

    Ok(OwnedCodecParams { ptr: codec_params })
}

impl CodecParamsPtrTrait for OwnedCodecParams {
    unsafe fn ptr(&self) -> *const AVCodecParameters {
        self.ptr
    }
}

impl Drop for OwnedCodecParams {
    fn drop(&mut self) {
        // SAFETY: The pointer is cast to *mut for avcodec_parameters_free
        // (which does not modify type-level data — only frees allocations).
        // The pointer is not used after this call.
        unsafe {
            avcodec_parameters_free(&mut (self.ptr as *mut AVCodecParameters));
        }
    }
}

// ── FLV codec ID mapping ──

/// Map a FLV video codec ID to an FFmpeg `AVCodecID`.
pub fn flv_video_codec_id_to_av(codec_id: u32) -> Result<AVCodecID> {
    match codec_id {
        // FLV AVC
        7 => Ok(AVCodecID::AV_CODEC_ID_H264),
        // FLV HEVC
        12 => Ok(AVCodecID::AV_CODEC_ID_HEVC),
        // Already in AVCodecID range
        x if x == AVCodecID::AV_CODEC_ID_H264 as u32 => Ok(AVCodecID::AV_CODEC_ID_H264),
        x if x == AVCodecID::AV_CODEC_ID_HEVC as u32 => Ok(AVCodecID::AV_CODEC_ID_HEVC),
        x => anyhow::bail!("Unsupported FLV video codec id: {}", x),
    }
}

/// Map a FLV audio codec ID (sound format) to an FFmpeg `AVCodecID`.
pub fn flv_audio_codec_id_to_av(codec_id: u32) -> Result<AVCodecID> {
    match codec_id {
        // FLV MP3
        2 | 14 => Ok(AVCodecID::AV_CODEC_ID_MP3),
        // FLV G.711 A-law / mu-law
        7 => Ok(AVCodecID::AV_CODEC_ID_PCM_ALAW),
        8 => Ok(AVCodecID::AV_CODEC_ID_PCM_MULAW),
        // FLV AAC
        10 => Ok(AVCodecID::AV_CODEC_ID_AAC),
        // Already in AVCodecID range
        x if x == AVCodecID::AV_CODEC_ID_AAC as u32 => Ok(AVCodecID::AV_CODEC_ID_AAC),
        x if x == AVCodecID::AV_CODEC_ID_MP3 as u32 => Ok(AVCodecID::AV_CODEC_ID_MP3),
        x if x == AVCodecID::AV_CODEC_ID_PCM_ALAW as u32 => Ok(AVCodecID::AV_CODEC_ID_PCM_ALAW),
        x if x == AVCodecID::AV_CODEC_ID_PCM_MULAW as u32 => Ok(AVCodecID::AV_CODEC_ID_PCM_MULAW),
        x => anyhow::bail!("Unsupported FLV audio codec id: {}", x),
    }
}

/// Map our pure-Rust `Codec` enum to an FFmpeg `AVCodecID`.
fn codec_to_av_codec_id(codec: Codec) -> AVCodecID {
    match codec {
        Codec::H264 => AVCodecID::AV_CODEC_ID_H264,
        Codec::H265 => AVCodecID::AV_CODEC_ID_HEVC,
        Codec::Aac => AVCodecID::AV_CODEC_ID_AAC,
        Codec::Mp3 => AVCodecID::AV_CODEC_ID_MP3,
        Codec::Opus => AVCodecID::AV_CODEC_ID_OPUS,
        Codec::Av1 => AVCodecID::AV_CODEC_ID_AV1,
    }
}

// ── Internal helpers ──

fn find_decoder(codec_id: AVCodecID) -> Result<*const AVCodec> {
    // SAFETY: avcodec_find_decoder returns a static codec descriptor or NULL.
    let codec = unsafe { avcodec_find_decoder(codec_id) };
    if codec.is_null() {
        anyhow::bail!("Unsupported codec ID: {:?}", codec_id);
    }
    Ok(codec)
}

fn alloc_codec_context(codec: *const AVCodec) -> Result<*mut AVCodecContext> {
    // SAFETY: avcodec_alloc_context3 returns a heap-allocated context or NULL.
    let ctx = unsafe { avcodec_alloc_context3(codec) };
    if ctx.is_null() {
        anyhow::bail!("Failed to allocate AVCodecContext");
    }
    Ok(ctx)
}

fn alloc_codec_params() -> Result<*mut AVCodecParameters> {
    // SAFETY: avcodec_parameters_alloc returns a heap-allocated params or NULL.
    let params = unsafe { avcodec_parameters_alloc() };
    if params.is_null() {
        anyhow::bail!("Failed to allocate AVCodecParameters");
    }
    Ok(params)
}

/// Copy extradata bytes into an `AVCodecParameters`'s extradata field.
///
/// Handles freeing any existing extradata first, then allocates a new
/// buffer with `AV_INPUT_BUFFER_PADDING_SIZE` extra bytes (FFmpeg convention).
pub(crate) fn copy_extradata_to_codecpar(
    codecpar: &mut impl CodecParamsMutPtrTrait,
    data: &[u8],
) -> Result<()> {
    // SAFETY: mut_ptr() returns a valid mutable pointer per trait contract.
    unsafe {
        let ptr = codecpar.mut_ptr();

        // Free existing extradata if any.
        if !(*ptr).extradata.is_null() {
            av_freep(&mut (*ptr).extradata as *mut _ as *mut std::ffi::c_void);
            (*ptr).extradata_size = 0;
        }

        let alloc_size = data
            .len()
            .checked_add(AV_INPUT_BUFFER_PADDING_SIZE as usize)
            .ok_or_else(|| anyhow::anyhow!("Extradata size overflow"))?;

        let extradata = av_mallocz(alloc_size) as *mut u8;
        if extradata.is_null() {
            anyhow::bail!("Failed to allocate codec extradata");
        }

        extradata.copy_from_nonoverlapping(data.as_ptr(), data.len());
        (*ptr).extradata = extradata;
        (*ptr).extradata_size = data.len() as i32;
    }

    Ok(())
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use livestream_core::types::CodecParams;

    #[test]
    fn from_codec_params_h264() {
        let params = CodecParams::new_video(Codec::H264, 90000, None);
        let owned = OwnedCodecParams::from_codec_params(&params).unwrap();
        assert!(owned.available());
    }

    #[test]
    fn from_codec_params_aac() {
        let params = CodecParams::new_audio(Codec::Aac, 44100, None);
        let owned = OwnedCodecParams::from_codec_params(&params).unwrap();
        assert!(owned.available());
    }

    #[test]
    fn from_codec_params_with_extradata() {
        let extradata = bytes::Bytes::from_static(&[0x01, 0x02, 0x03]);
        let params = CodecParams::new_video(Codec::H264, 90000, Some(extradata));
        let owned = OwnedCodecParams::from_codec_params(&params).unwrap();
        assert!(owned.available());
    }

    #[test]
    fn copy_from_roundtrip() {
        let params = CodecParams::new_video(Codec::H264, 90000, None);
        let original = OwnedCodecParams::from_codec_params(&params).unwrap();
        let copied = OwnedCodecParams::copy_from(&original).unwrap();
        assert!(copied.available());
        assert_eq!(original.codec_id(), copied.codec_id());
    }

    #[test]
    fn flv_video_mappings() {
        assert_eq!(
            flv_video_codec_id_to_av(7).unwrap(),
            AVCodecID::AV_CODEC_ID_H264
        );
        assert_eq!(
            flv_video_codec_id_to_av(12).unwrap(),
            AVCodecID::AV_CODEC_ID_HEVC
        );
    }

    #[test]
    fn flv_audio_mappings() {
        assert_eq!(
            flv_audio_codec_id_to_av(10).unwrap(),
            AVCodecID::AV_CODEC_ID_AAC
        );
        assert_eq!(
            flv_audio_codec_id_to_av(2).unwrap(),
            AVCodecID::AV_CODEC_ID_MP3
        );
    }

    #[test]
    fn create_dummy_video_succeeds() {
        let params =
            OwnedCodecParams::create_dummy_video(AVCodecID::AV_CODEC_ID_H264, 1920, 1080, 30.0)
                .unwrap();
        assert!(params.available());
        assert_eq!(params.codec_type(), AVMediaType::AVMEDIA_TYPE_VIDEO);
    }

    #[test]
    fn create_dummy_audio_succeeds() {
        let params =
            OwnedCodecParams::create_dummy_audio(AVCodecID::AV_CODEC_ID_AAC, 44100, 2).unwrap();
        assert!(params.available());
        assert_eq!(params.codec_type(), AVMediaType::AVMEDIA_TYPE_AUDIO);
    }
}
