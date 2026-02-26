use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct MemoryCache<T> {
    cache: Arc<RwLock<HashMap<String, T>>>,
}

impl<T> MemoryCache<T>
where
    T: Clone,
{
    pub fn new() -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn keys(&self) -> Vec<String> {
        self.cache.read().await.keys().cloned().collect()
    }

    pub async fn values(&self) -> Vec<T> {
        self.cache.read().await.values().cloned().collect()
    }

    pub async fn contains_key(&self, key: &str) -> bool {
        self.cache.read().await.contains_key(key)
    }

    pub async fn get(&self, key: &str) -> Option<T> {
        self.cache.read().await.get(key).cloned()
    }

    pub async fn get_or_insert_with<F>(&self, key: String, default: F) -> T
    where
        F: FnOnce() -> T,
    {
        let mut cache = self.cache.write().await;
        cache.entry(key).or_insert_with(default).clone()
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

impl<T> Default for MemoryCache<T>
where
    T: Clone,
{
    fn default() -> Self {
        Self::new()
    }
}
