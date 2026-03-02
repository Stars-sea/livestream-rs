//! Core FFmpeg wrapper modules for handling media streams.
//!
//! This module provides safe Rust abstractions over FFmpeg's C API for:
//! - Input/output context management
//! - Packet processing
//! - Stream information
//! - Context utilities

use std::ffi::{CStr, c_int};

use ffmpeg_sys_next::*;

pub mod context;
pub mod flv_parser;
pub mod input;
pub mod log;
pub mod options;
pub mod packet;
mod stream;

mod flv_output;
mod hls_output;

pub mod output {
    pub use super::flv_output::*;
    pub use super::hls_output::*;
}

/// Initializes FFmpeg network components.
/// Must be called before using network protocols like SRT.
pub fn init() {
    log::init_logging();
    unsafe {
        avformat_network_init();
    }
}

/// Converts an FFmpeg error code to a human-readable string.
///
/// # Arguments
/// * `code` - FFmpeg error code (negative value)
///
/// # Returns
/// Error message string
pub(super) fn ffmpeg_error(code: c_int) -> String {
    let mut buf = [0i8; 1024];
    unsafe {
        av_strerror(code, buf.as_mut_ptr(), buf.len());

        CStr::from_ptr(buf.as_mut_ptr())
            .to_string_lossy()
            .into_owned()
    }
}
