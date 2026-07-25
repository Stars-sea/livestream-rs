use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Mutex;

use crate::channel::{MpscTx, SendError};

// ── DemandSignal / DemandHandle ──

/// Tracks downstream demand from Sinks.
///
/// When created via `new()`, `is_wanted()` returns true only if at least one
/// `DemandHandle` exists.  When created via `new_always_wanted()`, always
/// returns true (used for permanently-active sinks like HLS recording).
pub struct DemandSignal {
    count: Arc<AtomicUsize>,
    always_wanted: bool,
}

impl DemandSignal {
    /// Create a signal that tracks handle count.
    pub fn new() -> Self {
        Self {
            count: Arc::new(AtomicUsize::new(0)),
            always_wanted: false,
        }
    }

    /// Create a signal that is always wanted, regardless of handles.
    /// For sinks like HLS recording that must always run.
    pub fn new_always_wanted() -> Self {
        Self {
            count: Arc::new(AtomicUsize::new(1)), // always-wanted reserves count=1 internally
            always_wanted: true,
        }
    }

    /// Returns true if there is at least one active handle, or if the signal
    /// was created with `new_always_wanted()`.
    pub fn is_wanted(&self) -> bool {
        self.always_wanted || self.count.load(Ordering::Acquire) > 0
    }

    /// Create a handle. While any handle exists, `is_wanted()` returns true
    /// (unless the signal was created with `new()` and no handles exist).
    pub fn new_handle(&self) -> DemandHandle {
        DemandHandle::from_signal(Arc::clone(&self.count))
    }
}

impl Default for DemandSignal {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for DemandSignal {
    fn clone(&self) -> Self {
        Self {
            count: Arc::clone(&self.count),
            always_wanted: self.always_wanted,
        }
    }
}

/// A handle representing a downstream consumer's demand.
///
/// Created via `DemandSignal::new_handle()` or `DemandHandle::empty()`.
/// Clone increments the reference count; Drop decrements it.
pub struct DemandHandle {
    count: Arc<AtomicUsize>,
    /// Whether this handle was created via `empty()` — in that case,
    /// it uses an isolated counter and clone/drop are no-ops for the
    /// signal's counter.
    detached: bool,
}

impl DemandHandle {
    /// Create a handle tied to a signal's counter.
    fn from_signal(count: Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::Release);
        Self {
            count,
            detached: false,
        }
    }

    /// Create an empty handle not connected to any DemandSignal.
    /// Clone and Drop are harmless no-ops on the signal counter.
    pub fn empty() -> Self {
        Self {
            count: Arc::new(AtomicUsize::new(0)),
            detached: true,
        }
    }
}

impl Clone for DemandHandle {
    fn clone(&self) -> Self {
        if !self.detached {
            self.count.fetch_add(1, Ordering::Release);
        }
        Self {
            count: Arc::clone(&self.count),
            detached: self.detached,
        }
    }
}

impl Drop for DemandHandle {
    fn drop(&mut self) {
        if !self.detached {
            self.count.fetch_sub(1, Ordering::Release);
        }
    }
}

// ── PadSender / PadReceiver ──

/// Backend for PadSender: either direct call or mpsc channel.
enum PadSenderBackend<T: 'static> {
    /// Direct: handler is called synchronously on send().
    /// Both nodes run in the same task.
    Direct {
        handler: Arc<dyn Fn(T) + Send + Sync>,
    },
    /// Channel: mpsc for cross-task communication.
    Channel { tx: MpscTx<T> },
}

impl<T: 'static> Clone for PadSenderBackend<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Direct { handler } => Self::Direct {
                handler: Arc::clone(handler),
            },
            Self::Channel { tx } => Self::Channel { tx: tx.clone() },
        }
    }
}

/// Sender side of a Pad. Owned by the upstream node.
pub struct PadSender<T: 'static> {
    inner: PadSenderBackend<T>,
    demand: DemandSignal,
}

impl<T> PadSender<T> {
    /// Create a direct-call pair. Items sent via `send()` are passed to `on_item`
    /// synchronously. Both sides must run in the same task.
    pub fn new_direct<F>(on_item: F) -> (Self, PadReceiver<T>)
    where
        F: Fn(T) + Send + Sync + 'static,
        T: Send + 'static,
    {
        let demand = DemandSignal::new();
        let sender = Self {
            inner: PadSenderBackend::Direct {
                handler: Arc::new(on_item),
            },
            demand: demand.clone(),
        };
        let receiver = PadReceiver {
            inner: PadReceiverBackend::Direct,
            demand,
        };
        (sender, receiver)
    }

    /// Create an mpsc channel pair for cross-task communication.
    pub fn new_channel(capacity: usize) -> (Self, PadReceiver<T>)
    where
        T: Send + 'static,
    {
        let (tx, rx) = crate::channel::mpsc("pad", None, capacity);
        let demand = DemandSignal::new();
        let sender = Self {
            inner: PadSenderBackend::Channel { tx },
            demand: demand.clone(),
        };
        let receiver: PadReceiver<T> = PadReceiver {
            inner: PadReceiverBackend::Channel { rx: Mutex::new(rx) },
            demand,
        };
        (sender, receiver)
    }

    /// Send an item downstream.
    ///
    /// For Direct backend: calls the handler synchronously.
    /// For Channel backend: sends via mpsc.
    pub fn send(&self, item: T) -> Result<(), SendError>
    where
        T: Send + 'static,
    {
        match &self.inner {
            PadSenderBackend::Direct { handler } => {
                handler(item);
                Ok(())
            }
            PadSenderBackend::Channel { tx } => tx.send(item),
        }
    }

    /// Reference to the demand signal (shared with the receiver).
    pub fn demand(&self) -> &DemandSignal {
        &self.demand
    }
}

impl<T: 'static> Clone for PadSender<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            demand: self.demand.clone(),
        }
    }
}

/// Backend for PadReceiver.
enum PadReceiverBackend<T: 'static> {
    /// Direct: items arrive via the handler passed to new_direct().
    Direct,
    /// Channel: polls the mpsc receiver, protected by a Mutex for &self access.
    Channel {
        rx: Mutex<crate::channel::MpscRx<T>>,
    },
}

/// Receiver side of a Pad. Owned by the downstream node.
pub struct PadReceiver<T: 'static> {
    inner: PadReceiverBackend<T>,
    // Needed for pipeline engine lazy-processing (Phase 4.1).
    #[allow(dead_code)]
    demand: DemandSignal,
}

impl<T> PadReceiver<T> {
    /// Receive the next item. Returns None if all senders are dropped.
    ///
    /// For Channel backend: locks the internal Mutex and polls the mpsc receiver.
    /// For Direct backend: panics — items arrive via the handler callback.
    pub async fn recv(&self) -> Option<T>
    where
        T: Send + 'static,
    {
        match &self.inner {
            PadReceiverBackend::Direct => {
                panic!(
                    "PadReceiver::recv() called on a Direct backend — \
                     items arrive via the handler callback, not polling"
                )
            }
            PadReceiverBackend::Channel { rx } => {
                let mut guard = rx.lock().await;
                guard.recv().await
            }
        }
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demand_new_is_not_wanted() {
        let sig = DemandSignal::new();
        assert!(!sig.is_wanted());
    }

    #[test]
    fn demand_with_handle_is_wanted() {
        let sig = DemandSignal::new();
        let h = sig.new_handle();
        assert!(sig.is_wanted());
        drop(h);
        assert!(!sig.is_wanted());
    }

    #[test]
    fn demand_always_wanted() {
        let sig = DemandSignal::new_always_wanted();
        assert!(sig.is_wanted());
        let h = sig.new_handle();
        drop(h);
        assert!(sig.is_wanted());
    }

    #[test]
    fn demand_multiple_handles() {
        let sig = DemandSignal::new();
        let h1 = sig.new_handle();
        let h2 = sig.new_handle();
        assert!(sig.is_wanted());
        drop(h1);
        assert!(sig.is_wanted());
        drop(h2);
        assert!(!sig.is_wanted());
    }

    #[test]
    fn demand_signal_clone_shares_state() {
        let sig1 = DemandSignal::new();
        let sig2 = sig1.clone();
        let h = sig1.new_handle();
        assert!(sig2.is_wanted());
        drop(h);
        assert!(!sig1.is_wanted());
    }

    #[test]
    fn demand_empty_handle_no_underflow() {
        let h = DemandHandle::empty();
        let h2 = h.clone();
        drop(h2);
        drop(h); // would underflow without the detached guard
    }

    #[tokio::test]
    async fn pad_channel_send_recv() {
        let (tx, rx) = PadSender::<i32>::new_channel(4);
        tx.send(42).unwrap();
        assert_eq!(rx.recv().await, Some(42));
        drop(tx);
        assert_eq!(rx.recv().await, None);
    }

    #[test]
    fn pad_direct_calls_handler() {
        use std::sync::Mutex;
        let called = Arc::new(Mutex::new(false));
        let called_clone = called.clone();
        let (_tx, _rx) = PadSender::<String>::new_direct(move |item| {
            assert_eq!(item, "hello");
            *called_clone.lock().unwrap() = true;
        });
        _tx.send("hello".into()).unwrap();
        assert!(*called.lock().unwrap());
    }
}
