use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use tokio::sync::{OnceCell, RwLock};

use crate::transport::ConnectionState;

use super::SessionDescriptor;

pub static REGISTRY: OnceCell<Arc<ConnectionRegistry>> = OnceCell::const_new();

pub fn init_registry() {
    REGISTRY.get_or_init(async || Arc::new(ConnectionRegistry::new()));
}

pub fn global_registry() -> Arc<ConnectionRegistry> {
    REGISTRY
        .get()
        .expect("Connection registry not initialized")
        .clone()
}

pub struct ConnectionRegistry {
    // Map of stream keys to active connections
    connections: DashMap<String, Arc<RwLock<SessionDescriptor>>>,
}

impl ConnectionRegistry {
    pub(self) fn new() -> Self {
        Self {
            connections: DashMap::new(),
        }
    }

    pub async fn add_session(&self, session: Arc<RwLock<SessionDescriptor>>) -> Result<()> {
        let stream_key = match session.read().await.id.clone() {
            Some(key) => key,
            None => anyhow::bail!("Connection does not have a valid stream key"),
        };
        self.connections.insert(stream_key, session);
        Ok(())
    }

    pub async fn remove_session(&self, session: Arc<RwLock<SessionDescriptor>>) {
        if let Some(stream_key) = session.read().await.id.clone() {
            self.connections.remove(&stream_key);
        }
    }

    pub fn remove(&self, stream_key: &str) {
        self.connections.remove(stream_key);
    }

    pub fn get(&self, stream_key: &str) -> Option<Arc<RwLock<SessionDescriptor>>> {
        self.connections
            .get(stream_key)
            .map(|entry| entry.value().clone())
    }

    pub async fn is_active(&self, stream_key: &str) -> bool {
        if let Some(session) = self.get(stream_key) {
            ConnectionState::Connected == session.read().await.state.into()
        } else {
            false
        }
    }
}
