use std::sync::{Arc, LazyLock};

use anyhow::Result;
use dashmap::{DashMap, Entry};
use livestream_core::traits::PipelineHandle;
use tokio::sync::RwLock;
use tokio::time::{Duration, sleep};
use tokio_util::sync::CancellationToken;

use super::state::*;

const SESSION_REMOVAL_GRACE_PERIOD: Duration = Duration::from_millis(200);

pub static INSTANCE: LazyLock<Arc<SessionRegistry>> =
    LazyLock::new(|| Arc::new(SessionRegistry::new()));

#[derive(Clone)]
struct SessionEntry {
    descriptor: Arc<RwLock<SessionDescriptor>>,
    cancel_token: CancellationToken,
    pipeline_handle: Option<PipelineHandle>,
}

pub struct SessionRegistry {
    sessions: Arc<DashMap<String, SessionEntry>>,
}
impl SessionRegistry {
    fn new() -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
        }
    }

    pub async fn register_session(
        &self,
        session: Arc<RwLock<SessionDescriptor>>,
        ct: CancellationToken,
    ) -> Result<()> {
        let stream_key = session.read().await.id.clone();

        match self.sessions.entry(stream_key.clone()) {
            Entry::Occupied(_) => {
                anyhow::bail!("Stream key {} is already in use", stream_key);
            }
            Entry::Vacant(entry) => {
                entry.insert(SessionEntry {
                    descriptor: session.clone(),
                    cancel_token: ct.clone(),
                    pipeline_handle: None,
                });
            }
        }

        let sessions = self.sessions.clone();
        let session_for_cleanup = session.clone();
        tokio::spawn(async move {
            ct.cancelled().await;

            {
                let mut descriptor = session_for_cleanup.write().await;
                descriptor.state = SessionState::Disconnected;
            }

            sleep(SESSION_REMOVAL_GRACE_PERIOD).await;
            sessions.remove(&stream_key);
        });

        Ok(())
    }

    fn get(&self, stream_key: &str) -> Option<SessionEntry> {
        self.sessions
            .get(stream_key)
            .map(|entry| entry.value().clone())
    }

    pub fn get_session(&self, stream_key: &str) -> Option<Arc<RwLock<SessionDescriptor>>> {
        self.get(stream_key).map(|entry| entry.descriptor.clone())
    }

    pub async fn get_descriptor(&self, stream_key: &str) -> Option<SessionDescriptor> {
        let session = self.get_session(stream_key)?;
        Some(session.read().await.clone())
    }

    pub async fn list_descriptors(&self) -> Vec<SessionDescriptor> {
        let sessions: Vec<Arc<RwLock<SessionDescriptor>>> = self
            .sessions
            .iter()
            .map(|entry| entry.value().descriptor.clone())
            .collect();

        let mut descriptors = Vec::with_capacity(sessions.len());
        for session in sessions {
            descriptors.push(session.read().await.clone());
        }

        descriptors
    }

    pub fn get_cancel_token(&self, stream_key: &str) -> Option<CancellationToken> {
        self.get(stream_key).map(|entry| entry.cancel_token.clone())
    }

    pub fn set_pipeline_handle(&self, stream_key: &str, handle: PipelineHandle) -> Result<()> {
        let mut entry = self
            .sessions
            .get_mut(stream_key)
            .ok_or_else(|| anyhow::anyhow!("No session found for stream key {}", stream_key))?;
        entry.pipeline_handle = Some(handle);
        Ok(())
    }

    pub fn get_pipeline_handle(&self, stream_key: &str) -> Option<PipelineHandle> {
        self.get(stream_key)
            .and_then(|entry| entry.pipeline_handle.clone())
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

        let mut session = session.write().await;
        let current_state = session.state;

        if current_state == SessionState::Disconnected {
            anyhow::bail!("Cannot update state of a disconnected session");
        }
        if current_state == new_state {
            return Ok(());
        }

        session.state = match new_state {
            SessionState::Connecting if current_state == SessionState::Pending => {
                SessionState::Connecting
            }
            SessionState::Connected if current_state == SessionState::Pending => {
                SessionState::Connected
            }
            SessionState::Connected if current_state == SessionState::Connecting => {
                SessionState::Connected
            }
            SessionState::Disconnected => SessionState::Disconnected,

            _ => {
                anyhow::bail!("Invalid state transition");
            }
        };
        Ok(())
    }
}
