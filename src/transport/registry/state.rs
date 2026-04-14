use crate::dispatcher::Protocol;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionEndpoint {
    pub port: Option<u16>,
    pub passphrase: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Pending,
    Connecting,
    Connected,
    Disconnected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionDescriptor {
    pub id: String,
    pub protocol: Protocol,
    pub endpoint: SessionEndpoint,
    pub state: SessionState,
}

impl SessionEndpoint {
    pub fn new(port: Option<u16>, passphrase: Option<String>) -> Self {
        Self { port, passphrase }
    }
}
