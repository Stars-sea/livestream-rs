use std::sync::Arc;

use anyhow::Result;
use dashmap::{DashMap, Entry};
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

static REGISTRY: OnceCell<Arc<CancelTokenRegistry>> = OnceCell::const_new();

pub async fn global_registry() -> Arc<CancelTokenRegistry> {
    REGISTRY
        .get_or_init(async || Arc::new(CancelTokenRegistry::new()))
        .await
        .clone()
}

pub struct CancelTokenRegistry {
    tokens: Arc<DashMap<String, CancellationToken>>,
}

impl CancelTokenRegistry {
    pub fn new() -> Self {
        Self {
            tokens: Arc::new(DashMap::new()),
        }
    }

    pub fn register(&self, stream_key: &str, token: CancellationToken) -> Result<()> {
        match self.tokens.entry(stream_key.to_string()) {
            Entry::Occupied(entry) if !entry.get().is_cancelled() => {
                anyhow::bail!("Stream key {} already has a cancellation token", stream_key)
            }
            Entry::Occupied(mut entry) => {
                entry.insert(token.clone());
            }
            Entry::Vacant(entry) => {
                entry.insert(token.clone());
            }
        }

        // Spawn a task to remove the token when it's cancelled
        let stream_key = stream_key.to_string();
        let tokens = self.tokens.clone();
        tokio::spawn(async move {
            token.cancelled().await;
            tokens.remove(&stream_key);
        });

        Ok(())
    }

    pub fn get(&self, stream_key: &str) -> Option<CancellationToken> {
        self.tokens
            .get(stream_key)
            .map(|entry| entry.value().clone())
    }
}
