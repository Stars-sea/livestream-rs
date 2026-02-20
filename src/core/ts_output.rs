//! MPEG-TS output context wrapper for FFmpeg.

use super::context::{Context, OutputContext, ffmpeg_error};
use super::input::SrtInputContext;

use anyhow::{Result, anyhow};
use ffmpeg_sys_next::*;

use std::ffi::{CString, c_int};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

/// Wrapper for FFmpeg output context configured for MPEG-TS files.
pub struct TsOutputContext {
    ctx: *mut AVFormatContext,
    path: PathBuf,
}

impl TsOutputContext {
    fn open_file(path: &PathBuf, flags: c_int) -> Result<*mut AVIOContext> {
        let mut pb: *mut AVIOContext = null_mut();
        let c_path = CString::new(path.as_path().display().to_string())?;

        let ret = unsafe { avio_open(&mut pb, c_path.as_ptr(), flags) };
        if ret < 0 {
            Err(anyhow!("Failed to open output file: {}", ffmpeg_error(ret)))
        } else {
            Ok(pb)
        }
    }

    pub fn create(path: &PathBuf, input_ctx: &SrtInputContext) -> Result<Self> {
        // Alloc output AVFormatContext
        let url = path.as_path().display().to_string();
        let output_ctx = Self::alloc_output_ctx("mpegts", &url)?;

        // Copy parameters of streams
        if let Err(e) = Self::copy_parameters(output_ctx, input_ctx) {
            unsafe { avformat_free_context(output_ctx) };
            return Err(e);
        }

        // Open file
        match Self::open_file(&path, AVIO_FLAG_WRITE) {
            Ok(pb) => unsafe { (*output_ctx).pb = pb },
            Err(e) => {
                unsafe { avformat_free_context(output_ctx) };
                return Err(e);
            }
        }

        // Write header
        if let Err(e) = Self::write_header(output_ctx) {
            unsafe { avformat_free_context(output_ctx) };
            return Err(e);
        }

        Ok(Self {
            ctx: output_ctx,
            path: path.clone(),
        })
    }

    pub fn create_segment<T: AsRef<Path>>(
        tmp_dir: T,
        input_ctx: &SrtInputContext,
        segment_id: u64,
    ) -> Result<Self> {
        let filename = format!("segment_{:04}.ts", segment_id);
        let path = PathBuf::from(tmp_dir.as_ref()).join(&filename);
        Self::create(&path, &input_ctx)
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for TsOutputContext {
    fn drop(&mut self) {
        if self.ctx.is_null() {
            return;
        }

        self.write_trailer().ok();
        unsafe {
            avio_closep(&mut (*self.ctx).pb);
            avformat_free_context(self.ctx);
        }
        self.ctx = null_mut();
    }
}

impl Context for TsOutputContext {
    fn get_ctx(&self) -> *mut AVFormatContext {
        self.ctx
    }
}

impl OutputContext for TsOutputContext {}
