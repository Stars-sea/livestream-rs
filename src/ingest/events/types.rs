use std::{fmt::Display, path::PathBuf};

#[derive(Clone, Debug)]
pub enum StreamMessage {
    SegmentComplete {
        live_id: String,
        // segment_id: String,
        path: PathBuf,
    },

    IngestWorkerStarted {
        live_id: String,
    },

    IngestWorkerStopped {
        live_id: String,
    },

    StreamStarted {
        live_id: String,
    },

    StreamStopped {
        live_id: String,
        error: Option<String>,
    },

    StreamRestarting {
        live_id: String,
        error: String,
    },
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
                    error.as_ref().unwrap_or(&"None".to_string())
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
