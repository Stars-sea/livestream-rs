use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use tokio::sync::{OnceCell, RwLock};

use crate::transport::{ConnectionState, SessionDescriptor};

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
                let state = value.read().await.state.into();
                if ConnectionState::Disconnected != state && ConnectionState::Precreate != state {
                    anyhow::bail!("Session for stream key {} is still active", stream_key);
                }
                Ok((key, value))
            }
            None => anyhow::bail!("No session found for stream key {}", stream_key),
        }
    }

    pub fn get(&self, stream_key: &str) -> Option<Arc<RwLock<SessionDescriptor>>> {
        self.connections
            .get(stream_key)
            .map(|entry| entry.value().clone())
    }
}
