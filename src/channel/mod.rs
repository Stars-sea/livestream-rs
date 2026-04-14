use crossfire::{AsyncRx, MTx, mpsc};
use tokio::sync::broadcast;

mod error;
mod receiver;
mod sender;

pub use error::*;

pub type MpscTx<T> = sender::Sender<MTx<mpsc::Array<T>>>;
pub type MpscRx<T> = receiver::Receiver<AsyncRx<mpsc::Array<T>>>;

pub type BroadcastTx<T> = sender::Sender<broadcast::Sender<T>>;
pub type BroadcastRx<T> = receiver::Receiver<broadcast::Receiver<T>>;

pub fn mpsc<T: Send + 'static>(
    queue: impl Into<String>,
    live_id: impl Into<Option<String>>,
    size: usize,
) -> (MpscTx<T>, MpscRx<T>) {
    let (tx, rx) = mpsc::bounded_blocking_async(size);

    let queue = queue.into();
    let live_id = live_id.into();

    let tx = MpscTx::new(tx, &queue, live_id.clone());
    let rx = MpscRx::new(rx, &queue, live_id);
    (tx, rx)
}

pub fn broadcast<T: Clone + Send + 'static>(
    queue: impl Into<String>,
    live_id: impl Into<Option<String>>,
    size: usize,
) -> (BroadcastTx<T>, BroadcastRx<T>) {
    let (tx, rx) = broadcast::channel(size);

    let queue = queue.into();
    let live_id = live_id.into();

    let tx = BroadcastTx::new(tx, &queue, live_id.clone());
    let rx = BroadcastRx::new(rx, &queue, live_id);
    (tx, rx)
}
