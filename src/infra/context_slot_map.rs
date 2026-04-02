use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::Mutex;

pub struct ContextSlotMap<T> {
    inner: Arc<DashMap<String, Arc<Mutex<Option<T>>>>>,
}

impl<T> Clone for ContextSlotMap<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Default for ContextSlotMap<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> ContextSlotMap<T> {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    pub fn get_or_create(&self, key: &str) -> Arc<Mutex<Option<T>>> {
        self.inner
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone()
    }

    pub fn remove(&self, key: &str) -> Option<Arc<Mutex<Option<T>>>> {
        self.inner.remove(key).map(|(_, slot)| slot)
    }

    pub async fn with_or_create<R, F>(&self, key: &str, f: F) -> R
    where
        F: FnOnce(&mut Option<T>) -> R,
    {
        let slot = self.get_or_create(key);
        let mut guard = slot.lock().await;
        f(&mut guard)
    }

    pub async fn with_removed<R, F>(&self, key: &str, f: F) -> Option<R>
    where
        F: FnOnce(&mut Option<T>) -> R,
    {
        let slot = self.remove(key)?;
        let mut guard = slot.lock().await;
        Some(f(&mut guard))
    }
}
