use std::{fmt::Display, path::PathBuf};

#[derive(Clone, Debug)]
pub enum StreamMessage {
    SegmentComplete {
        live_id: String,
        // segment_id: String,
        path: PathBuf,
    },

    PullerStarted {
        live_id: String,
    },

    PullerStopped {
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

    pub fn puller_started(live_id: &str) -> Self {
        StreamMessage::PullerStarted {
            live_id: live_id.to_string(),
        }
    }

    pub fn puller_stopped(live_id: &str) -> Self {
        StreamMessage::PullerStopped {
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
            StreamMessage::PullerStarted { live_id } => {
                write!(f, "PullerStarted: live_id={}", live_id)
            }
            StreamMessage::PullerStopped { live_id } => {
                write!(f, "PullerStopped: live_id={}", live_id)
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
