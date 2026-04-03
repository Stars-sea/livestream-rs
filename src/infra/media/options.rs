use std::{ffi::CStr, ptr::null_mut};

use ffmpeg_sys_next::*;

pub trait StreamOptions {
    fn filename(&self) -> String;

    fn to_dict(&self) -> *mut AVDictionary;
}

fn dict_set(dict: *mut *mut AVDictionary, key: &str, value: &str) {
    let Ok(c_key) = CStr::from_bytes_until_nul(key.as_bytes()) else {
        return;
    };
    let Ok(c_value) = CStr::from_bytes_until_nul(value.as_bytes()) else {
        return;
    };
    unsafe {
        av_dict_set(dict, c_key.as_ptr(), c_value.as_ptr(), 0);
    }
}

fn dict_set_int(dict: *mut *mut AVDictionary, key: &str, value: i64) {
    let Ok(c_key) = CStr::from_bytes_until_nul(key.as_bytes()) else {
        return;
    };
    unsafe {
        av_dict_set_int(dict, c_key.as_ptr(), value, 0);
    }
}

#[derive(Debug)]
pub struct SrtInputStreamOptions {
    port: u16,

    live_id: String,
    passphrase: Option<String>,
}

#[allow(unused)]
impl SrtInputStreamOptions {
    pub fn new(port: u16, live_id: String, passphrase: Option<String>) -> Self {
        Self {
            port,
            live_id,
            passphrase,
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn passphrase(&self) -> Option<&str> {
        self.passphrase.as_deref()
    }
}

impl StreamOptions for SrtInputStreamOptions {
    fn filename(&self) -> String {
        format!("srt://:{}", self.port)
    }

    fn to_dict(&self) -> *mut AVDictionary {
        let mut dict: *mut AVDictionary = null_mut();

        dict_set(&mut dict, "mode", "listener");
        dict_set(&mut dict, "srt_streamid", &self.live_id);

        if let Some(passphrase) = &self.passphrase {
            dict_set_int(&mut dict, "enforced_encryption", 1);
            dict_set(&mut dict, "passphrase", &passphrase);
        }

        // Set UDP receive buf to 5MB to accommodate higher latency and prevent buffer underruns,
        // which can help improve stream stability in less-than-ideal network conditions
        // See: https://ffmpeg.org/ffmpeg-protocols.html#toc-srt
        dict_set_int(&mut dict, "recv_buffer_size", 5242880);

        dict_set_int(&mut dict, "ffs", 5242880);
        dict_set_int(&mut dict, "rcvbuf", 5242880);

        // Set smoother to "live" to optimize for live streaming scenarios, which can help reduce latency and improve stream stability
        dict_set(&mut dict, "smoother", "live");

        // In order to not exceed the bandwidth with the overhead transmission (retransmitted and control packets).
        dict_set(&mut dict, "transtype", "live");

        // Set overrun_nonfatal to allow the stream to continue even if packets are lost,
        // which can happen in high-latency or unstable network conditions
        // See: https://svn.ffmpeg.org/ffmpeg-protocols.html#toc-udp
        dict_set_int(&mut dict, "overrun_nonfatal", 1);

        // Set RW timeout to 30s to prevent blocking forever if the connection drops silently.
        // Value is in microseconds.
        dict_set_int(&mut dict, "timeout", 30000000);

        dict
    }
}
