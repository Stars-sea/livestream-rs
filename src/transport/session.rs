#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Precreate,
    Created,
    Connecting,
    Connected,
    Disconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Srt(ConnectionState),
    Rtmp(ConnectionState),
}

pub struct SessionDescriptor {
    pub id: String,
    pub state: SessionState,
}

impl Into<ConnectionState> for SessionState {
    fn into(self) -> ConnectionState {
        match self {
            SessionState::Srt(state) => state,
            SessionState::Rtmp(state) => state,
        }
    }
}
