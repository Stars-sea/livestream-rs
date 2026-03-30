use std::ptr::null_mut;

use ffmpeg_sys_next::*;

#[allow(drop_bounds)]
pub trait StreamOptions {
    fn filename(&self) -> String;

    fn to_dict(&self) -> *mut AVDictionary;
}

fn dict_set(dict: *mut *mut AVDictionary, key: &str, value: &str) {
    let c_key = std::ffi::CString::new(key).unwrap();
    let c_value = std::ffi::CString::new(value).unwrap();
    unsafe {
        av_dict_set(dict, c_key.as_ptr(), c_value.as_ptr(), 0);
    }
}

fn dict_set_int(dict: *mut *mut AVDictionary, key: &str, value: i64) {
    let c_key = std::ffi::CString::new(key).unwrap();
    unsafe {
        av_dict_set_int(dict, c_key.as_ptr(), value, 0);
    }
}

#[derive(Debug)]
pub struct SrtInputStreamOptions {
    host: String,
    port: u16,

    live_id: String,
    passphrase: String,
}

impl SrtInputStreamOptions {
    pub fn new(host: String, port: u16, live_id: String, passphrase: String) -> Self {
        Self {
            host,
            port,
            live_id,
            passphrase,
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn passphrase(&self) -> &str {
        &self.passphrase
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
        dict_set(&mut dict, "passphrase", &self.passphrase);
        dict_set_int(&mut dict, "enforced_encryption", 1);

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
