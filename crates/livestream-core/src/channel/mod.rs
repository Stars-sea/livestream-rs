mod broadcast;
mod error;
mod mpsc;

pub use error::*;

use std::sync::Arc;

use crossfire::mpsc as cf_mpsc;
use tokio::sync::broadcast as tk_broadcast;

use crate::channel::broadcast as broadcast_impl;
use crate::channel::mpsc as mpsc_impl;

/// Multi-producer, single-consumer channel sender.
pub type MpscTx<T> = mpsc_impl::MpscSender<T>;
/// Multi-producer, single-consumer channel receiver.
pub type MpscRx<T> = broadcast::MpscReceiver<T>;

/// Broadcast channel sender (tokio::sync::broadcast).
pub type BroadcastTx<T> = broadcast_impl::BroadcastSender<T>;
/// Broadcast channel receiver (tokio::sync::broadcast).
pub type BroadcastRx<T> = broadcast_impl::BroadcastReceiver<T>;

/// Create a bounded MPSC channel.
pub fn mpsc<T: Send + 'static>(
    queue: &'static str,
    live_id: Option<&str>,
    capacity: usize,
) -> (MpscTx<T>, MpscRx<T>) {
    let (tx, rx) = cf_mpsc::bounded_blocking_async(capacity);
    let live_id: Option<Arc<str>> = live_id.map(|s| s.into());

    let tx = MpscTx::new(tx, queue, live_id.clone());
    let rx = MpscRx::new(rx, queue, live_id);
    (tx, rx)
}

/// Create a broadcast channel.
pub fn broadcast<T: Clone + Send + 'static>(
    queue: &'static str,
    live_id: Option<&str>,
    capacity: usize,
) -> (BroadcastTx<T>, BroadcastRx<T>) {
    let (tx, rx) = tk_broadcast::channel(capacity);
    let live_id: Option<Arc<str>> = live_id.map(|s| s.into());

    let tx = BroadcastTx::new(tx, queue, live_id.clone());
    let rx = BroadcastRx::new(rx);
    (tx, rx)
}
