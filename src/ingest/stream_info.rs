use std::path::Path;

use anyhow::Result;
use tempfile::TempDir;

use crate::core::options::SrtInputStreamOptions;
use crate::settings::load_settings;

#[derive(Debug)]
pub struct StreamInfo {
    live_id: String,

    srt_options: SrtInputStreamOptions,

    /// Temporary directory for storing segments before they are sent to the MinIO.
    /// The directory will be automatically deleted when the StreamInfo is dropped
    cache_dir: TempDir,
    segment_duration: i32,
}

impl StreamInfo {
    pub fn new(live_id: String, port: u16, passphrase: String) -> Result<Self> {
        let host = load_settings().ingest.host.clone();
        let segment_duration = load_settings().ingest.duration;

        let srt_options = SrtInputStreamOptions::new(host, port, live_id.clone(), passphrase);

        let cache_dir = tempfile::tempdir()?;

        Ok(StreamInfo {
            live_id,
            srt_options,
            cache_dir,
            segment_duration,
        })
    }

    pub fn live_id(&self) -> &str {
        &self.live_id
    }

    pub fn srt_options(&self) -> &SrtInputStreamOptions {
        &self.srt_options
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
}
