use crossfire::{AsyncTxTrait, MAsyncTx, MTx, TrySendError, Tx, mpmc, mpsc, spsc};
use tokio::sync::broadcast;

pub(super) enum SendError<T> {
    Full(T),
    Disconnected(T),
    NoSubscriber(T),
}

pub(super) trait ChannelTx<T> {
    fn channel_send(&self, item: T) -> Result<(), SendError<T>>;
}

impl<T> ChannelTx<T> for Tx<spsc::Array<T>>
where
    T: Send + 'static,
{
    fn channel_send(&self, item: T) -> Result<(), SendError<T>> {
        match self.try_send(item) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(v)) => Err(SendError::Full(v)),
            Err(TrySendError::Disconnected(v)) => Err(SendError::Disconnected(v)),
        }
    }
}

impl<T> ChannelTx<T> for &Tx<spsc::Array<T>>
where
    T: Send + 'static,
{
    fn channel_send(&self, item: T) -> Result<(), SendError<T>> {
        (*self).channel_send(item)
    }
}

impl<T> ChannelTx<T> for MTx<mpsc::Array<T>>
where
    T: Send + 'static,
{
    fn channel_send(&self, item: T) -> Result<(), SendError<T>> {
        match self.try_send(item) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(v)) => Err(SendError::Full(v)),
            Err(TrySendError::Disconnected(v)) => Err(SendError::Disconnected(v)),
        }
    }
}

impl<T> ChannelTx<T> for &MTx<mpsc::Array<T>>
where
    T: Send + 'static,
{
    fn channel_send(&self, item: T) -> Result<(), SendError<T>> {
        (*self).channel_send(item)
    }
}

impl<T> ChannelTx<T> for MTx<mpmc::Array<T>>
where
    T: Send + 'static,
{
    fn channel_send(&self, item: T) -> Result<(), SendError<T>> {
        match self.try_send(item) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(v)) => Err(SendError::Full(v)),
            Err(TrySendError::Disconnected(v)) => Err(SendError::Disconnected(v)),
        }
    }
}

impl<T> ChannelTx<T> for &MTx<mpmc::Array<T>>
where
    T: Send + 'static,
{
    fn channel_send(&self, item: T) -> Result<(), SendError<T>> {
        (*self).channel_send(item)
    }
}

impl<T> ChannelTx<T> for MAsyncTx<mpsc::Array<T>>
where
    T: Send + 'static + Unpin,
{
    fn channel_send(&self, item: T) -> Result<(), SendError<T>> {
        match self.try_send(item) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(v)) => Err(SendError::Full(v)),
            Err(TrySendError::Disconnected(v)) => Err(SendError::Disconnected(v)),
        }
    }
}

impl<T> ChannelTx<T> for &MAsyncTx<mpsc::Array<T>>
where
    T: Send + 'static + Unpin,
{
    fn channel_send(&self, item: T) -> Result<(), SendError<T>> {
        (*self).channel_send(item)
    }
}

impl<T: Clone + Send + 'static> ChannelTx<T> for broadcast::Sender<T> {
    fn channel_send(&self, item: T) -> Result<(), SendError<T>> {
        match self.send(item) {
            Ok(_) => Ok(()),
            Err(e) => Err(SendError::NoSubscriber(e.0)),
        }
    }
}

impl<T: Clone + Send + 'static> ChannelTx<T> for &broadcast::Sender<T> {
    fn channel_send(&self, item: T) -> Result<(), SendError<T>> {
        (*self).channel_send(item)
    }
}
