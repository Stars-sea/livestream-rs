use std::path::Path;

use anyhow::Result;
use tempfile::TempDir;

use crate::media::options::SrtInputStreamOptions;

/// Protocol-specific ingest endpoint/options bound to one stream.
///
/// Responsibilities:
/// - Carry transport-specific connection parameters for adapter startup.
/// - Keep protocol branching data localized to one enum.
///
/// Out of scope:
/// - No runtime socket ownership.
/// - No lifecycle state tracking.
#[derive(Debug)]
pub enum StreamInputOptions {
    /// Settings to establish and read from an SRT connection.
    Srt(SrtInputStreamOptions),
    /// Settings bound to an RTMP media stream.
    Rtmp {
        host: String,
        port: u16,
        appname: String,
    },
}

/// Immutable per-stream runtime metadata shared across ingest components.
///
/// Responsibilities:
/// - Provide stream identity and input options to workers/adapters.
/// - Own temporary cache directory lifecycle for segment artifacts.
/// - Expose segmenting configuration used by processing loops.
///
/// Out of scope:
/// - No worker execution logic.
/// - No manager command/state transitions.
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
    pub fn new_srt(
        live_id: String,
        host: String,
        segment_duration: i32,
        port: u16,
        passphrase: String,
    ) -> Result<Self> {
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

    pub fn new_rtmp(
        live_id: String,
        host: String,
        port: u16,
        appname: String,
        segment_duration: i32,
    ) -> Result<Self> {
        let input_options = StreamInputOptions::Rtmp {
            host,
            port,
            appname,
        };

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
            StreamInputOptions::Rtmp { .. } => None,
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
