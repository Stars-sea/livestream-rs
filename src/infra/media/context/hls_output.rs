//! MPEG-TS output context wrapper for FFmpeg.

use crate::infra::media::stream::StreamCollection;

use super::{Context, OutputContext};

use anyhow::Result;
use ffmpeg_sys_next::*;
use tracing::warn;

use std::path::{Path, PathBuf};
use std::ptr::null_mut;

/// Wrapper for FFmpeg output context configured for MPEG-TS files.
#[derive(Debug)]
pub struct HlsOutputContext {
    ctx: *mut AVFormatContext,
    path: PathBuf,
}

impl HlsOutputContext {
    pub fn create(path: &PathBuf, streams: &dyn StreamCollection) -> Result<Self> {
        // Alloc output AVFormatContext
        let url = path.as_path().display().to_string();
        let output_ctx = Self::alloc_output_ctx("mpegts", Some(&url))?;

        // Copy parameters of streams
        if let Err(e) = Self::copy_streams(output_ctx, streams) {
            unsafe { avformat_free_context(output_ctx) };
            return Err(e);
        }

        if unsafe { (*output_ctx).pb.is_null() } {
            // Open file
            match Self::open_io(null_mut(), Some(&url), AVIO_FLAG_WRITE) {
                Ok(pb) => unsafe { (*output_ctx).pb = pb },
                Err(e) => {
                    unsafe { avformat_free_context(output_ctx) };
                    return Err(e);
                }
            }
        }

        // Write header
        if let Err(e) = Self::write_header(output_ctx) {
            unsafe {
                if !(*output_ctx).pb.is_null() {
                    avio_closep(&mut (*output_ctx).pb);
                }
                avformat_free_context(output_ctx)
            };
            return Err(e);
        }

        Ok(Self {
            ctx: output_ctx,
            path: path.clone(),
        })
    }

    pub fn create_segment<T: AsRef<Path>>(
        tmp_dir: T,
        streams: &dyn StreamCollection,
        segment_id: u64,
    ) -> Result<Self> {
        let filename = format!("segment_{:04}.ts", segment_id);
        let path = PathBuf::from(tmp_dir.as_ref()).join(&filename);
        Self::create(&path, streams)
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

unsafe impl Send for HlsOutputContext {}

impl Drop for HlsOutputContext {
    fn drop(&mut self) {
        if self.ctx.is_null() {
            return;
        }

        if let Err(e) = self.write_trailer() {
            warn!(error = %e, path = %self.path.display(), "Failed to write HLS trailer during context drop");
        }
        unsafe {
            avio_closep(&mut (*self.ctx).pb);
            avformat_free_context(self.ctx);
        }
        self.ctx = null_mut();
    }
}

impl Context for HlsOutputContext {
    unsafe fn ptr(&self) -> *mut AVFormatContext {
        self.ctx
    }
}

impl OutputContext for HlsOutputContext {}
