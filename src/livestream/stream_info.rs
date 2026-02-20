use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::settings::Settings;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StreamInfo {
    live_id: String,
    host: String,
    port: u16,

    passphrase: String,

    cache_dir: PathBuf,
    segment_duration: i32,
}

impl StreamInfo {
    pub fn new(live_id: String, port: u16, passphrase: String, settings: &Settings) -> Self {
        let host = settings.host.clone();
        let cache_dir = PathBuf::from(&settings.cache_dir).join(&live_id);
        let segment_duration = settings.segment_time;
        Self {
            live_id,
            host,
            port,
            passphrase,
            cache_dir,
            segment_duration,
        }
    }

    pub fn live_id(&self) -> &str {
        &self.live_id
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

    pub fn cache_dir(&self) -> &Path {
        self.cache_dir.as_path()
    }

    pub fn segment_duration(&self) -> i32 {
        self.segment_duration
    }

    pub fn listener_url(&self) -> String {
        format!(
            "srt://:{}?mode=listener&passphrase={}&srt_streamid={}",
            self.port, self.passphrase, self.live_id
        )
    }

    pub fn rtmp_url(&self) -> String {
        format!("rtmp://{}:{}/lives/{}", self.host, self.port, self.live_id)
    }
}
