use std::path::Path;

use anyhow::Result;
use tempfile::TempDir;

use crate::core::options::{RtmpInputStreamOptions, SrtInputStreamOptions};
use crate::settings::load_settings;

#[derive(Debug)]
pub enum StreamInputOptions {
    Srt(SrtInputStreamOptions),
    Rtmp(RtmpInputStreamOptions),
}

#[derive(Debug)]
pub struct StreamInfo {
    live_id: String,

    input_options: StreamInputOptions,

    /// Temporary directory for storing segments before they are sent to the MinIO.
    /// The directory will be automatically deleted when the StreamInfo is dropped
    cache_dir: TempDir,
    segment_duration: i32,
}

impl StreamInfo {
    pub fn new_srt(live_id: String, port: u16, passphrase: String) -> Result<Self> {
        let settings = load_settings();
        let host = settings.ingest.host.clone();
        let segment_duration = settings.ingest.duration;

        let input_options =
            StreamInputOptions::Srt(SrtInputStreamOptions::new(host, port, live_id.clone(), passphrase));

        let cache_dir = tempfile::tempdir()?;

        Ok(StreamInfo {
            live_id,
            input_options,
            cache_dir,
            segment_duration,
        })
    }

    pub fn new_rtmp(live_id: String) -> Result<Self> {
        let settings = load_settings();
        let host = settings.ingest.host.clone();
        let port = settings.publish.port;
        let appname = settings.publish.appname.clone();
        let segment_duration = settings.ingest.duration;

        let input_options =
            StreamInputOptions::Rtmp(RtmpInputStreamOptions::new(host, port, appname, live_id.clone()));

        let cache_dir = tempfile::tempdir()?;

        Ok(StreamInfo {
            live_id,
            input_options,
            cache_dir,
            segment_duration,
        })
    }

    pub fn live_id(&self) -> &str {
        &self.live_id
    }

    pub fn input_options(&self) -> &StreamInputOptions {
        &self.input_options
    }

    pub fn srt_options(&self) -> Option<&SrtInputStreamOptions> {
        match &self.input_options {
            StreamInputOptions::Srt(options) => Some(options),
            StreamInputOptions::Rtmp(_) => None,
        }
    }

    pub fn rtmp_options(&self) -> Option<&RtmpInputStreamOptions> {
        match &self.input_options {
            StreamInputOptions::Srt(_) => None,
            StreamInputOptions::Rtmp(options) => Some(options),
        }
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
