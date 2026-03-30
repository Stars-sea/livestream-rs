pub trait ConnectionStateTrait {
    fn is_active(&self) -> bool;

    fn is_stopped(&self) -> bool;
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

    fn is_stopped(&self) -> bool {
        matches!(self, RtmpState::Disconnected)
    }
}

impl ConnectionStateTrait for SrtState {
    fn is_active(&self) -> bool {
        matches!(self, SrtState::Connected)
    }

    fn is_stopped(&self) -> bool {
        matches!(self, SrtState::Disconnected)
    }
}

impl ConnectionStateTrait for SessionState {
    fn is_active(&self) -> bool {
        match self {
            SessionState::Rtmp(state) => state.is_active(),
            SessionState::Srt(state) => state.is_active(),
        }
    }

    fn is_stopped(&self) -> bool {
        match self {
            SessionState::Rtmp(state) => state.is_stopped(),
            SessionState::Srt(state) => state.is_stopped(),
        }
    }
}
