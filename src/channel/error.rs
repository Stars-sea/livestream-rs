use std::fmt::Display;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    Closed,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecvError {
    Lagged(u64),
    Closed,
}

impl Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Closed => write!(f, "Channel is closed"),
            SendError::Full => write!(f, "Channel is full"),
        }
    }
}

impl Display for RecvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecvError::Lagged(n) => write!(f, "Receiver lagged and skipped {} messages", n),
            RecvError::Closed => write!(f, "Channel is closed"),
        }
    }
}
