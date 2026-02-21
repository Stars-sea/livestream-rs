use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct MemoryCache<T> {
    cache: Arc<RwLock<HashMap<String, T>>>,
}

impl<T> MemoryCache<T> {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn keys(&self) -> Vec<String> {
        self.cache.read().await.keys().cloned().collect()
    }

    pub async fn get(&self, key: &str) -> Option<T>
    where
        T: Clone,
    {
        self.cache.read().await.get(key).cloned()
    }

    pub async fn set(&self, key: String, value: T) -> Result<()> {
        if self.cache.read().await.contains_key(&key) {
            anyhow::bail!("Key already exists in cache: {key}");
        }
        self.cache.write().await.insert(key, value);
        Ok(())
    }

    pub async fn remove(&self, key: &str) {
        self.cache.write().await.remove(key);
    }
}
