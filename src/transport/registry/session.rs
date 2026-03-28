use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use tokio::sync::{OnceCell, RwLock};

use crate::transport::{
    ConnectionStateTrait, RtmpState, SessionDescriptor, SessionState, SrtState,
};

static REGISTRY: OnceCell<Arc<ConnectionRegistry>> = OnceCell::const_new();

pub async fn global_registry() -> Arc<ConnectionRegistry> {
    REGISTRY
        .get_or_init(async || Arc::new(ConnectionRegistry::new()))
        .await
        .clone()
}

pub struct ConnectionRegistry {
    // Map of stream keys to active connections
    connections: DashMap<String, Arc<RwLock<SessionDescriptor>>>,
}

impl ConnectionRegistry {
    fn new() -> Self {
        Self {
            connections: DashMap::new(),
        }
    }

    pub async fn register_session(&self, session: Arc<RwLock<SessionDescriptor>>) -> Result<()> {
        let stream_key = session.read().await.id.clone();
        if self.connections.contains_key(&stream_key) {
            anyhow::bail!("Stream key {} is already in use", stream_key);
        }

        self.connections.insert(stream_key, session);
        Ok(())
    }

    pub async fn remove_session(
        &self,
        session: Arc<RwLock<SessionDescriptor>>,
    ) -> Result<(String, Arc<RwLock<SessionDescriptor>>)> {
        let id = &session.read().await.id;
        self.remove(id).await
    }

    pub async fn remove(
        &self,
        stream_key: &str,
    ) -> Result<(String, Arc<RwLock<SessionDescriptor>>)> {
        match self.connections.remove(stream_key) {
            Some((key, value)) => {
                if value.read().await.state.is_active() {
                    anyhow::bail!("Session for stream key {} is still active", stream_key);
                }
                Ok((key, value))
            }
            None => anyhow::bail!("No session found for stream key {}", stream_key),
        }
    }

    fn get_session(&self, stream_key: &str) -> Option<Arc<RwLock<SessionDescriptor>>> {
        self.connections
            .get(stream_key)
            .map(|entry| entry.value().clone())
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
