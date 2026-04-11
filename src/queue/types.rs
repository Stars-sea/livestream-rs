#[derive(Debug, Clone, Copy, Default)]
pub struct NoTx;

#[derive(Debug, Clone, Copy, Default)]
pub struct NoRx;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelSendStatus {
    Sent,
    Full,
    Disconnected,
}
