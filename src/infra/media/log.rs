use std::ffi::{c_char, c_int, c_void};

use ffmpeg_sys_next::*;
use tracing::{Level, debug, error, info, trace, warn};

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
    unsafe { av_log_set_level(c_level) }
}

/// Disables all FFmpeg logging output.
#[allow(unused)]
pub fn set_log_quiet() {
    unsafe { av_log_set_level(AV_LOG_QUIET) }
}

pub(super) fn init_logging() {
    unsafe { av_log_set_callback(Some(log_callback)) };
}

#[cfg(any(target_os = "linux", target_os = "android"))]
type AvLogVaList = *mut __va_list_tag;

#[cfg(not(any(target_os = "linux", target_os = "android")))]
type AvLogVaList = va_list;

unsafe fn format_log_line(
    ptr: *mut c_void,
    level: c_int,
    fmt: *const c_char,
    vl: AvLogVaList,
    line: *mut c_char,
    line_size: c_int,
    print_prefix: *mut c_int,
) -> c_int {
    unsafe { av_log_format_line2(ptr, level, fmt, vl, line, line_size, print_prefix) }
}

unsafe extern "C" fn log_callback(
    ptr: *mut c_void,
    level: c_int,
    fmt: *const c_char,
    vl: AvLogVaList,
) {
    if fmt.is_null() {
        return;
    }

    let mut line = [0 as c_char; 1024];
    let mut print_prefix = 1;

    let format_result = unsafe {
        format_log_line(
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

    let log_message = unsafe { std::ffi::CStr::from_ptr(line.as_ptr()) }
        .to_string_lossy()
        .trim_end()
        .to_string();

    if log_message.is_empty() {
        return;
    }

    let level = match level {
        AV_LOG_FATAL => Level::ERROR,
        AV_LOG_PANIC => Level::ERROR,
        AV_LOG_ERROR => Level::ERROR,
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
}
