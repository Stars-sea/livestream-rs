//! Core FFmpeg wrapper modules for handling media streams.
//!
//! This module provides safe Rust abstractions over FFmpeg's C API for:
//! - Input/output context management
//! - Packet processing
//! - Stream information
//! - Context utilities

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
