use std::sync::{Arc, Mutex};

use crossfire::{AsyncRx, MAsyncRx, MTx, Tx, mpmc, mpsc, spsc};
use tokio::sync::broadcast;

mod channel;
mod error;
mod send;
mod stream;
mod types;

pub use channel::Channel;
#[allow(unused_imports)]
pub use error::SubscribeError;
pub use stream::{BroadcastRecv, ChannelStream};
#[allow(unused_imports)]
pub use types::{ChannelSendStatus, NoRx, NoTx};

#[allow(dead_code)]
pub type SpscChannel<T> = Channel<Tx<spsc::Array<T>>, Arc<Mutex<Option<AsyncRx<spsc::Array<T>>>>>>;
pub type MpscChannel<T> = Channel<MTx<mpsc::Array<T>>, Arc<Mutex<Option<AsyncRx<mpsc::Array<T>>>>>>;
#[allow(dead_code)]
pub type MpmcChannel<T> = Channel<MTx<mpmc::Array<T>>, MAsyncRx<mpmc::Array<T>>>;
pub type BroadcastChannel<T> = Channel<broadcast::Sender<T>, NoRx>;
