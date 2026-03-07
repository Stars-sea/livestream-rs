use std::path::Path;

use anyhow::Result;
use tempfile::TempDir;

use crate::config::load_config;
use crate::media::options::{RtmpInputStreamOptions, SrtInputStreamOptions};

/// Provides the connection and configuration details specific to the protocol
/// selected for media ingest.
#[derive(Debug)]
pub enum StreamInputOptions {
    /// Settings to establish and read from an SRT connection.
    Srt(SrtInputStreamOptions),
    /// Settings bound to an RTMP media stream.
    Rtmp(RtmpInputStreamOptions),
}

/// Contains identifying information, configuration, and transient disk states 
/// utilized during a media stream's reception and processing phase.
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
        let config = load_config();
        let host = config.ingest.host.clone();
        let segment_duration = config.ingest.duration;

        let input_options = StreamInputOptions::Srt(SrtInputStreamOptions::new(
            host,
            port,
            live_id.clone(),
            passphrase,
        ));

        let cache_dir = tempfile::tempdir()?;

        Ok(StreamInfo {
            live_id,
            input_options,
            cache_dir,
            segment_duration,
        })
    }

    #[cfg(test)]
    pub fn new_test(live_id: String, duration: i32) -> Self {
        StreamInfo {
            live_id: live_id.clone(),
            input_options: StreamInputOptions::Rtmp(RtmpInputStreamOptions::new(
                "localhost".to_string(),
                1935,
                "app".to_string(),
                live_id,
            )),
            cache_dir: tempfile::tempdir().unwrap(),
            segment_duration: duration,
        }
    }

    pub fn new_rtmp(live_id: String) -> Result<Self> {
        let config = load_config();
        let host = config.ingest.host.clone();
        let port = config.egress.port;
        let appname = config.egress.appname.clone();
        let segment_duration = config.ingest.duration;

        let input_options = StreamInputOptions::Rtmp(RtmpInputStreamOptions::new(
            host,
            port,
            appname,
            live_id.clone(),
        ));

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
