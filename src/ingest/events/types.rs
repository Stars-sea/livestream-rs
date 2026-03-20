use std::{fmt::Display, path::PathBuf};

/// Represents significant lifecycle and operational events originating from active streams.
/// Handlers process these events to trigger side-effects like callbacks, cache updates,
/// and telemetry observations.
#[derive(Clone, Debug)]
pub enum StreamMessage {
    /// A media segment (e.g., an HLS chunk) has finished writing to disk.
    SegmentComplete { live_id: String, path: PathBuf },

    /// The background worker thread managing stream ingest has begun processing.
    IngestWorkerStarted { live_id: String },

    /// The background worker thread managing stream ingest has cleanly terminated.
    IngestWorkerStopped { live_id: String },

    /// Media data is actively flowing for the given stream.
    StreamStarted { live_id: String },

    /// The media flow has ceased, either cleanly or due to an error.
    StreamStopped {
        live_id: String,
        error: Option<String>,
    },

    /// The stream encountered an error and is attempting to reconnect / restart.
    StreamRestarting { live_id: String, error: String },
}

impl StreamMessage {
    pub fn segment_complete(live_id: &str, path: &PathBuf) -> Self {
        let path = path.clone();
        StreamMessage::SegmentComplete {
            live_id: live_id.to_string(),
            // segment_id: path.file_name().unwrap().display().to_string(),
            path,
        }
    }

    pub fn ingest_worker_started(live_id: &str) -> Self {
        StreamMessage::IngestWorkerStarted {
            live_id: live_id.to_string(),
        }
    }

    pub fn ingest_worker_stopped(live_id: &str) -> Self {
        StreamMessage::IngestWorkerStopped {
            live_id: live_id.to_string(),
        }
    }

    pub fn stream_started(live_id: &str) -> Self {
        StreamMessage::StreamStarted {
            live_id: live_id.to_string(),
        }
    }

    pub fn stream_stopped(live_id: &str, error: Option<String>) -> Self {
        StreamMessage::StreamStopped {
            live_id: live_id.to_string(),
            error,
        }
    }

    pub fn stream_restarting(live_id: &str, error: String) -> Self {
        StreamMessage::StreamRestarting {
            live_id: live_id.to_string(),
            error,
        }
    }
}

impl Display for StreamMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamMessage::SegmentComplete { live_id, .. } => {
                write!(f, "SegmentComplete: live_id={}", live_id)
            }
            StreamMessage::IngestWorkerStarted { live_id } => {
                write!(f, "IngestWorkerStarted: live_id={}", live_id)
            }
            StreamMessage::IngestWorkerStopped { live_id } => {
                write!(f, "IngestWorkerStopped: live_id={}", live_id)
            }
            StreamMessage::StreamStarted { live_id } => {
                write!(f, "StreamStarted: live_id={}", live_id)
            }
            StreamMessage::StreamStopped { live_id, error } => {
                write!(
                    f,
                    "StreamStopped: live_id={}, error={}",
                    live_id,
                    error.as_deref().unwrap_or(&"None")
                )
            }
            StreamMessage::StreamRestarting { live_id, error } => {
                write!(f, "StreamRestarting: live_id={}, error={}", live_id, error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::StreamMessage;

    #[test]
    fn constructors_build_expected_variants() {
        let segment = StreamMessage::segment_complete("live_1", &PathBuf::from("a.ts"));
        let started = StreamMessage::stream_started("live_1");
        let stopped = StreamMessage::stream_stopped("live_1", None);
        let restarting = StreamMessage::stream_restarting("live_1", "err".to_string());
        let worker_started = StreamMessage::ingest_worker_started("live_1");
        let worker_stopped = StreamMessage::ingest_worker_stopped("live_1");

        assert!(matches!(segment, StreamMessage::SegmentComplete { .. }));
        assert!(matches!(started, StreamMessage::StreamStarted { .. }));
        assert!(matches!(stopped, StreamMessage::StreamStopped { .. }));
        assert!(matches!(restarting, StreamMessage::StreamRestarting { .. }));
        assert!(matches!(
            worker_started,
            StreamMessage::IngestWorkerStarted { .. }
        ));
        assert!(matches!(
            worker_stopped,
            StreamMessage::IngestWorkerStopped { .. }
        ));
    }

    #[test]
    fn display_contains_variant_names() {
        let msg = StreamMessage::stream_stopped("live_1", Some("boom".to_string()));
        let text = msg.to_string();

        assert!(text.contains("StreamStopped"));
        assert!(text.contains("live_1"));
        assert!(text.contains("boom"));
    }
}
