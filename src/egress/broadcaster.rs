#![allow(dead_code)]

use std::{fmt::Debug, hash::Hash, sync::Arc};

use anyhow::Result;
use crossfire::{AsyncRx, MTx, mpsc, select::SelectResult};
use dashmap::DashMap;

#[derive(Clone, Debug)]
pub(crate) struct Broadcaster<
    K: Clone + Eq + Hash + Debug + Send + Sync + 'static,
    T: Clone + Send + 'static,
> {
    senders: Arc<DashMap<K, Vec<MTx<mpsc::List<T>>>>>,
}

impl<K: Clone + Eq + Hash + Debug + Send + Sync + 'static, T: Clone + Send + 'static>
    Broadcaster<K, T>
{
    pub fn new() -> Self {
        Self {
            senders: Arc::new(DashMap::new()),
        }
    }

    pub fn subscribe(&self, id: K) -> BroadcastRx<T> {
        let (tx, rx) = mpsc::unbounded_async();
        self.senders
            .entry(id.clone())
            .or_insert_with(|| Vec::new())
            .push(tx);

        let senders = Arc::clone(&self.senders);

        BroadcastRx::new(rx, move || {
            if let Some(mut stream_senders) = senders.get_mut(&id) {
                stream_senders.retain(|tx| !tx.is_disconnected());
            }
        })
    }

    pub fn send(&self, id: K, item: T) -> Result<usize> {
        let mut delivered: usize = 0;

        if let Some(senders) = self.senders.get(&id) {
            for tx in senders.iter() {
                if tx.send(item.clone()).is_ok() {
                    delivered += 1;
                }
            }
        };

        if delivered == 0 {
            anyhow::bail!("No subscribers for id: {:?}", id);
        } else {
            Ok(delivered)
        }
    }
}

pub struct BroadcastRx<T>
where
    T: Clone + Send + 'static,
{
    pub rx: AsyncRx<mpsc::List<T>>,
    on_drop: Option<Box<dyn FnOnce() + Send + 'static>>,
}

impl<T: Clone + Send + 'static> BroadcastRx<T> {
    pub(crate) fn new<F>(rx: AsyncRx<mpsc::List<T>>, on_drop: F) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        Self {
            rx,
            on_drop: Some(Box::new(on_drop)),
        }
    }

    #[inline(always)]
    pub async fn recv(&mut self) -> Result<T, crossfire::RecvError> {
        self.rx.recv().await
    }

    #[inline]
    pub async fn recv_timeout(
        &mut self,
        duration: std::time::Duration,
    ) -> Result<T, crossfire::RecvTimeoutError> {
        self.rx.recv_timeout(duration).await
    }

    #[inline]
    pub async fn recv_with_timer<FR, R>(
        &mut self,
        sleep: FR,
    ) -> Result<T, crossfire::RecvTimeoutError>
    where
        FR: Future<Output = R>,
    {
        self.rx.recv_with_timer(sleep).await
    }

    #[inline(always)]
    pub fn try_recv(&mut self) -> Result<T, crossfire::TryRecvError> {
        self.rx.try_recv()
    }

    #[inline(always)]
    pub fn read_select(&mut self, result: SelectResult) -> Result<T, crossfire::RecvError> {
        self.rx.read_select(result)
    }
}

impl<T: Clone + Send + 'static> Drop for BroadcastRx<T> {
    fn drop(&mut self) {
        if let Some(on_drop) = self.on_drop.take() {
            on_drop();
        }
    }
}
