use std::path::PathBuf;

#[derive(Clone, Copy, Debug)]
pub enum Protocal {
    Rtmp,
    Srt,
}

#[derive(Clone, Debug)]
pub enum SessionEvent {
    SessionStarted { live_id: String, protocal: Protocal },

    // TODO: add more fields, such as error code, error message, etc.
    SessionEnded { live_id: String, protocal: Protocal },

    SegmentComplete { live_id: String, path: PathBuf },
}
