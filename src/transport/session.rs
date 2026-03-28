pub trait ConnectionStateTrait {
    fn is_active(&self) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RtmpState {
    Pending,
    Connecting,
    Connected,
    Disconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SrtState {
    Pending,
    Connected,
    Disconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Srt(SrtState),
    Rtmp(RtmpState),
}

pub struct SessionDescriptor {
    pub id: String,
    pub state: SessionState,
}

impl ConnectionStateTrait for RtmpState {
    fn is_active(&self) -> bool {
        matches!(self, RtmpState::Connected)
    }
}

impl ConnectionStateTrait for SrtState {
    fn is_active(&self) -> bool {
        matches!(self, SrtState::Connected)
    }
}

impl ConnectionStateTrait for SessionState {
    fn is_active(&self) -> bool {
        match self {
            SessionState::Rtmp(state) => state.is_active(),
            SessionState::Srt(state) => state.is_active(),
        }
    }
}
