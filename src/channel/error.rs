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
