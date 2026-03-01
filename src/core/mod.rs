//! Core FFmpeg wrapper modules for handling media streams.
//!
//! This module provides safe Rust abstractions over FFmpeg's C API for:
//! - Input/output context management
//! - Packet processing
//! - Stream information
//! - Context utilities

use ffmpeg_sys_next::*;
use tracing::Level;

pub mod context;
pub mod flv_parser;
pub mod input;
pub mod options;
pub mod packet;
mod stream;

mod flv_output;
mod hls_output;

pub mod output {
    pub use super::flv_output::*;
    pub use super::hls_output::*;
}

/// Sets the FFmpeg logging level based on Rust log levels.
#[allow(dead_code)]
pub fn set_log_level(level: Level) {
    let c_level = match level {
        Level::ERROR => AV_LOG_ERROR,
        Level::WARN => AV_LOG_WARNING,
        Level::INFO => AV_LOG_INFO,
        Level::DEBUG => AV_LOG_INFO,
        Level::TRACE => AV_LOG_TRACE,
    };
    unsafe { av_log_set_level(c_level) }
}

/// Disables all FFmpeg logging output.
pub fn set_log_quiet() {
    unsafe { av_log_set_level(AV_LOG_QUIET) }
}

/// Initializes FFmpeg network components.
/// Must be called before using network protocols like SRT.
pub fn init() {
    unsafe {
        avformat_network_init();
    }
}
