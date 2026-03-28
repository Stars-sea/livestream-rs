use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use tokio::sync::{OnceCell, RwLock};

use super::SessionDescriptor;

pub mod global {
    use super::*;

    pub async fn register_session(session: Arc<RwLock<SessionDescriptor>>) -> Result<()> {
        global_registry().await.register_session(session).await
    }

    pub async fn remove_session(
        session: Arc<RwLock<SessionDescriptor>>,
    ) -> Option<(String, Arc<RwLock<SessionDescriptor>>)> {
        global_registry().await.remove_session(session).await
    }

    pub async fn get_session(stream_key: &str) -> Option<Arc<RwLock<SessionDescriptor>>> {
        global_registry().await.get(stream_key)
    }
}

static REGISTRY: OnceCell<Arc<ConnectionRegistry>> = OnceCell::const_new();

// TODO: Wrap methods of registry in a mod
async fn global_registry() -> Arc<ConnectionRegistry> {
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
        self.connections.insert(stream_key, session);
        Ok(())
    }

    pub async fn remove_session(
        &self,
        session: Arc<RwLock<SessionDescriptor>>,
    ) -> Option<(String, Arc<RwLock<SessionDescriptor>>)> {
        let stream_key = session.read().await.id.clone();
        self.connections.remove(&stream_key)
    }

    pub fn remove(&self, stream_key: &str) -> Option<(String, Arc<RwLock<SessionDescriptor>>)> {
        self.connections.remove(stream_key)
    }

    pub fn get(&self, stream_key: &str) -> Option<Arc<RwLock<SessionDescriptor>>> {
        self.connections
            .get(stream_key)
            .map(|entry| entry.value().clone())
    }
}
