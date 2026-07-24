//! livestream-media: FFmpeg RAII wrappers.
//!
//! This is the **only** crate with `unsafe` FFmpeg access. All other crates
//! interact with media through typed Rust types (`Packet`, `Frame`, `OwnedCodecParams`,
//! `EncodedPacket`, etc.).
//!
//! ## Modules
//!
//! | Module | Purpose |
//! |--------|---------|
//! | `packet` | `Packet` — AVPacket RAII |
//! | `frame` | `Frame` — AVFrame RAII |
//! | `codec` | `OwnedCodecParams` — AVCodecParameters RAII |
//! | `decoder` | `Decoder` — AVCodecContext RAII (decode direction) |
//! | `encoder` | `Encoder` — AVCodecContext RAII (encode direction) |
//! | `scaler` | `Scaler` — SwsContext RAII |
//! | `stream` | `StreamCollection` trait + `StaticStreamCollection` |
//! | `convert` | `EncodedPacket` ↔ `Packet` bridge |
//! | `flv` | `FlvTag` types + encoding + packetizer |
//! | `context` | `Context` / `OutputContext` traits + `HlsOutputContext` |
//! | `bsf` | `H264Mp4ToAnnexb` bitstream filter |

use std::ffi::{CStr, c_int};

use ffmpeg_sys_next::*;
use tracing::{Level, debug, error, info, trace, warn};

pub mod bsf;
pub mod codec;
pub mod context;
pub mod convert;
pub mod decoder;
pub mod encoder;
pub mod flv;
pub mod frame;
pub mod packet;
pub mod scaler;
pub mod stream;

// ── FFmpeg init ──

/// Initializes FFmpeg network components.
/// Must be called before using network protocols.
pub fn init() {
    init_logging();
    // SAFETY: avformat_network_init is safe to call once at startup.
    unsafe {
        avformat_network_init();
    }
}

// ── Error formatting ──

/// Converts an FFmpeg error code to a human-readable string.
pub fn ffmpeg_error(code: c_int) -> String {
    let mut buf = [0i8; 1024];
    // SAFETY: av_strerror writes to the buffer with a known size.
    unsafe {
        av_strerror(code, buf.as_mut_ptr(), buf.len() as _);
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }
}

// ── FFmpeg logging ──

/// Sets the FFmpeg logging level based on Rust log levels.
#[allow(unused)]
pub fn set_log_level(level: Level) {
    let c_level = match level {
        Level::ERROR => AV_LOG_ERROR,
        Level::WARN => AV_LOG_WARNING,
        Level::INFO => AV_LOG_INFO,
        Level::DEBUG => AV_LOG_DEBUG,
        Level::TRACE => AV_LOG_TRACE,
    };
    // SAFETY: av_log_set_level is safe to call at any time.
    unsafe {
        av_log_set_level(c_level);
    }
}

/// Disables all FFmpeg logging.
#[allow(unused)]
pub fn set_log_quiet() {
    // SAFETY: av_log_set_level is safe to call at any time.
    unsafe {
        av_log_set_level(AV_LOG_QUIET);
    }
}

fn init_logging() {
    // SAFETY: Setting the log callback is safe. The callback body is wrapped
    // in catch_unwind so that a panic (e.g., OOM during formatting) cannot
    // unwind across the extern "C" boundary.
    unsafe {
        av_log_set_callback(Some(log_callback));
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
type AvLogVaList = *mut __va_list_tag;

#[cfg(not(any(target_os = "linux", target_os = "android")))]
type AvLogVaList = va_list;

/// FFmpeg log callback.  Wraps the fallible formatting in `catch_unwind`
/// so that a panic (OOM allocation in `to_string()`) cannot unwind across
/// the C FFI boundary, which would be undefined behavior.
unsafe extern "C" fn log_callback(
    ptr: *mut std::ffi::c_void,
    level: c_int,
    fmt: *const std::ffi::c_char,
    vl: AvLogVaList,
) {
    // SAFETY: catch_unwind prevents panics from unwinding into C.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if fmt.is_null() {
            return;
        }

        let mut line = [0 as std::ffi::c_char; 1024];
        let mut print_prefix = 1;

        let format_result = unsafe {
            av_log_format_line2(
                ptr,
                level,
                fmt,
                vl,
                line.as_mut_ptr(),
                line.len() as c_int,
                &mut print_prefix,
            )
        };

        if format_result < 0 {
            return;
        }

        let log_message = unsafe { CStr::from_ptr(line.as_ptr()) }
            .to_string_lossy()
            .trim_end()
            .to_string();

        if log_message.is_empty() {
            return;
        }

        let level = match level {
            AV_LOG_FATAL | AV_LOG_PANIC | AV_LOG_ERROR => Level::ERROR,
            AV_LOG_WARNING => Level::WARN,
            AV_LOG_INFO => Level::INFO,
            AV_LOG_DEBUG => Level::DEBUG,
            AV_LOG_TRACE => Level::TRACE,
            _ => Level::INFO,
        };

        match level {
            Level::ERROR => error!(target: "ffmpeg", "{}", log_message),
            Level::WARN => warn!(target: "ffmpeg", "{}", log_message),
            Level::INFO => info!(target: "ffmpeg", "{}", log_message),
            Level::DEBUG => debug!(target: "ffmpeg", "{}", log_message),
            Level::TRACE => trace!(target: "ffmpeg", "{}", log_message),
        }
    }));
}
