use std::path::PathBuf;

#[derive(Clone, Debug)]
pub enum StreamControlMessage {
    // StartStream {
    //     live_id: String,
    //     port: u16,
    //     passphrase: String,
    // },
    StopStream { live_id: String },
}

impl StreamControlMessage {
    pub fn is_stop_stream(&self) -> bool {
        matches!(self, StreamControlMessage::StopStream { .. })
    }

    pub fn live_id(&self) -> &str {
        match self {
            StreamControlMessage::StopStream { live_id } => live_id,
        }
    }

    pub fn stop_stream(live_id: &str) -> Self {
        StreamControlMessage::StopStream {
            live_id: live_id.to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum StreamMessage {
    SegmentComplete {
        live_id: String,
        segment_id: String,
        path: PathBuf,
    },

    StreamStarted {
        live_id: String,
    },

    StreamStopped {
        live_id: String,
        error: Option<String>,
        path: PathBuf,
    },
}

impl StreamMessage {
    pub fn live_id(&self) -> &str {
        match self {
            StreamMessage::SegmentComplete { live_id, .. } => live_id,
            StreamMessage::StreamStarted { live_id } => live_id,
            StreamMessage::StreamStopped { live_id, .. } => live_id,
        }
    }

    pub fn segment_complete(live_id: &str, path: &PathBuf) -> Self {
        let path = path.clone();
        StreamMessage::SegmentComplete {
            live_id: live_id.to_string(),
            segment_id: path.file_name().unwrap().display().to_string(),
            path,
        }
    }

    pub fn stream_started(live_id: &str) -> Self {
        StreamMessage::StreamStarted {
            live_id: live_id.to_string(),
        }
    }

    pub fn stream_stopped(live_id: &str, error: Option<String>, path: &PathBuf) -> Self {
        StreamMessage::StreamStopped {
            live_id: live_id.to_string(),
            error,
            path: path.clone(),
        }
    }
}
