use std::sync::Arc;

use anyhow::Result;
use dashmap::{DashMap, Entry};
use tokio::sync::{OnceCell, RwLock};
use tokio::time::{Duration, sleep};
use tokio_util::sync::CancellationToken;

use crate::transport::contract::state::*;

const SESSION_REMOVAL_GRACE_PERIOD: Duration = Duration::from_millis(200);

static REGISTRY: OnceCell<Arc<ConnectionRegistry>> = OnceCell::const_new();

pub async fn global_registry() -> Arc<ConnectionRegistry> {
    REGISTRY
        .get_or_init(async || Arc::new(ConnectionRegistry::new()))
        .await
        .clone()
}

pub type SessionEntry = (Arc<RwLock<SessionDescriptor>>, CancellationToken);

pub struct ConnectionRegistry {
    // Map of stream keys to active connections
    connections: Arc<DashMap<String, SessionEntry>>,
}

impl ConnectionRegistry {
    fn new() -> Self {
        Self {
            connections: Arc::new(DashMap::new()),
        }
    }

    pub async fn register_session(
        &self,
        session: Arc<RwLock<SessionDescriptor>>,
        ct: CancellationToken,
    ) -> Result<()> {
        let stream_key = session.read().await.id.clone();

        match self.connections.entry(stream_key.clone()) {
            Entry::Occupied(_) => {
                anyhow::bail!("Stream key {} is already in use", stream_key);
            }
            Entry::Vacant(entry) => {
                entry.insert((session.clone(), ct.clone()));
            }
        }

        let connections = self.connections.clone();
        let session_for_cleanup = session.clone();
        tokio::spawn(async move {
            ct.cancelled().await;

            {
                let mut descriptor = session_for_cleanup.write().await;
                descriptor.state = match descriptor.protocol {
                    SessionProtocol::Rtmp => SessionState::Rtmp(RtmpState::Disconnected),
                    SessionProtocol::Srt => SessionState::Srt(SrtState::Disconnected),
                };
            }

            // Keep a short tombstone window so polling-based watchers can observe
            // the final disconnected state before the session entry disappears.
            sleep(SESSION_REMOVAL_GRACE_PERIOD).await;
            connections.remove(&stream_key);
        });

        Ok(())
    }

    pub async fn remove_session(
        &self,
        session: Arc<RwLock<SessionDescriptor>>,
    ) -> Result<(String, SessionEntry)> {
        let id = &session.read().await.id;
        self.remove(id).await
    }

    pub async fn remove(&self, stream_key: &str) -> Result<(String, SessionEntry)> {
        match self.connections.entry(stream_key.to_string()) {
            Entry::Occupied(entry) => {
                let (value, _) = entry.get().clone();
                if value.read().await.state.is_active() {
                    anyhow::bail!("Session for stream key {} is still active", stream_key);
                }

                Ok(entry.remove_entry())
            }
            Entry::Vacant(_) => anyhow::bail!("No session found for stream key {}", stream_key),
        }
    }

    fn get(&self, stream_key: &str) -> Option<SessionEntry> {
        self.connections
            .get(stream_key)
            .map(|entry| entry.value().clone())
    }

    pub fn get_session(&self, stream_key: &str) -> Option<Arc<RwLock<SessionDescriptor>>> {
        self.get(stream_key).map(|entry| entry.0.clone())
    }

    pub async fn get_descriptor(&self, stream_key: &str) -> Option<SessionDescriptor> {
        let session = self.get_session(stream_key)?;
        Some(session.read().await.clone())
    }

    pub async fn list_descriptors(&self) -> Vec<SessionDescriptor> {
        let sessions: Vec<Arc<RwLock<SessionDescriptor>>> = self
            .connections
            .iter()
            .map(|entry| entry.value().0.clone())
            .collect();

        let mut descriptors = Vec::with_capacity(sessions.len());
        for session in sessions {
            descriptors.push(session.read().await.clone());
        }

        descriptors
    }

    pub fn get_cancel_token(&self, stream_key: &str) -> Option<CancellationToken> {
        self.get(stream_key).map(|entry| entry.1.clone())
    }

    pub async fn get_state(&self, stream_key: &str) -> Option<SessionState> {
        match self.get_session(stream_key) {
            Some(session) => Some(session.read().await.state),
            None => None,
        }
    }

    pub async fn update_state(&self, stream_key: &str, new_state: SessionState) -> Result<()> {
        let session = match self.get_session(stream_key) {
            Some(session) => session,
            None => anyhow::bail!("No session found for stream key {}", stream_key),
        };

        let current_state = session.read().await.state;
        match current_state {
            SessionState::Rtmp(state) => Self::update_rtmp_state(session, state, new_state).await?,
            SessionState::Srt(state) => Self::update_srt_state(session, state, new_state).await?,
        };

        Ok(())
    }

    async fn update_rtmp_state(
        session: Arc<RwLock<SessionDescriptor>>,
        current_state: RtmpState,
        new_state: SessionState,
    ) -> Result<()> {
        let new_state = match new_state {
            SessionState::Rtmp(state) => state,
            _ => anyhow::bail!("Invalid state type"),
        };

        session.write().await.state = match new_state {
            RtmpState::Connecting if current_state == RtmpState::Pending => {
                SessionState::Rtmp(RtmpState::Connecting)
            }
            RtmpState::Connected if current_state == RtmpState::Connecting => {
                SessionState::Rtmp(RtmpState::Connected)
            }
            RtmpState::Disconnected if current_state == RtmpState::Connected => {
                SessionState::Rtmp(RtmpState::Disconnected)
            }
            RtmpState::Disconnected if current_state == RtmpState::Connecting => {
                SessionState::Rtmp(RtmpState::Disconnected)
            }
            _ => {
                anyhow::bail!("Invalid state transition");
            }
        };
        Ok(())
    }

    async fn update_srt_state(
        session: Arc<RwLock<SessionDescriptor>>,
        current_state: SrtState,
        new_state: SessionState,
    ) -> Result<()> {
        let new_state = match new_state {
            SessionState::Srt(state) => state,
            _ => anyhow::bail!("Invalid state type"),
        };

        session.write().await.state = match new_state {
            SrtState::Connected if current_state == SrtState::Pending => {
                SessionState::Srt(SrtState::Connected)
            }
            SrtState::Disconnected if current_state == SrtState::Connected => {
                SessionState::Srt(SrtState::Disconnected)
            }
            SrtState::Disconnected if current_state == SrtState::Pending => {
                SessionState::Srt(SrtState::Disconnected)
            }
            _ => {
                anyhow::bail!("Invalid state transition");
            }
        };
        Ok(())
    }
}
