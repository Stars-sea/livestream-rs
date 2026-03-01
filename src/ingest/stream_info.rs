use std::path::Path;

use anyhow::Result;
use tempfile::TempDir;

use crate::settings::IngestConfig;

#[derive(Debug)]
pub struct StreamInfo {
    live_id: String,
    host: String,
    srt_port: u16,

    passphrase: String,

    /// Temporary directory for storing segments before they are sent to the MinIO.
    /// The directory will be automatically deleted when the StreamInfo is dropped
    cache_dir: TempDir,
    segment_duration: i32,
}

impl StreamInfo {
    pub fn new(
        live_id: String,
        port: u16,
        passphrase: String,
        config: &IngestConfig,
    ) -> Result<Self> {
        let host = config.host.clone();
        let segment_duration = config.duration;

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

    pub fn is_cache_empty(&self) -> Result<bool> {
        let mut entries = self.cache_dir.path().read_dir()?;
        Ok(entries.next().is_none())
    }

    pub fn segment_duration(&self) -> i32 {
        self.segment_duration
    }

    pub fn srt_listener_url(&self) -> String {
        format!(
            "srt://:{}?mode=listener&passphrase={}&srt_streamid={}&latency=500000&timeout=5000000",
            self.srt_port, self.passphrase, self.live_id
        )
    }
}
