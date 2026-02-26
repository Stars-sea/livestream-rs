use std::path::Path;

use anyhow::Result;
use tempfile::TempDir;

use crate::Settings;

#[derive(Debug)]
pub struct StreamInfo {
    live_id: String,
    host: String,
    srt_port: u16,

    passphrase: String,

    cache_dir: TempDir,
    segment_duration: i32,
}

impl StreamInfo {
    pub fn new(
        live_id: String,
        port: u16,
        passphrase: String,
        settings: &Settings,
    ) -> Result<Self> {
        let host = settings.host.clone();
        let segment_duration = settings.segment_time;

        let cache_dir = tempfile::tempdir()?;

        Ok(StreamInfo {
            live_id,
            host,
            srt_port: port,
            passphrase,
            cache_dir,
            segment_duration,
        })
    }

    pub fn live_id(&self) -> &str {
        &self.live_id
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn srt_port(&self) -> u16 {
        self.srt_port
    }

    pub fn passphrase(&self) -> &str {
        &self.passphrase
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir.path()
    }

    pub fn segment_duration(&self) -> i32 {
        self.segment_duration
    }

    pub fn srt_listener_url(&self) -> String {
        format!(
            "srt://:{}?mode=listener&passphrase={}&srt_streamid={}",
            self.srt_port, self.passphrase, self.live_id
        )
    }
}
