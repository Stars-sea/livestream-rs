use std::sync::Arc;

use anyhow::Result;
use crossfire::{MTx, mpsc, oneshot};

use crate::ingest::StreamInfo;
use crate::ingest::adapters::RtmpTag;

/// Internal command protocol between `StreamManager` and `ManagerActor`.
///
/// Responsibilities:
/// - Represent all state-changing and query operations for stream management.
/// - Carry a typed reply channel for request/response style coordination.
///
/// Out of scope:
/// - No business logic, validation, or side effects.
pub(super) enum ManagerCommand {
    MakeSrtStreamInfo {
        live_id: String,
        passphrase: String,
        reply: oneshot::TxOneshot<Result<Arc<StreamInfo>>>,
    },
    MakeRtmpStreamInfo {
        live_id: String,
        reply: oneshot::TxOneshot<Result<Arc<StreamInfo>>>,
    },
    StartSrtStream {
        info: Arc<StreamInfo>,
        reply: oneshot::TxOneshot<Result<()>>,
    },
    StartRtmpStream {
        info: Arc<StreamInfo>,
        reply: oneshot::TxOneshot<Result<()>>,
    },
    StopStream {
        live_id: String,
        reply: oneshot::TxOneshot<Result<()>>,
    },
    RemoveStream {
        live_id: String,
        reply: oneshot::TxOneshot<()>,
    },
    Shutdown {
        reply: oneshot::TxOneshot<()>,
    },
    ListActiveStreams {
        reply: oneshot::TxOneshot<Result<Vec<String>>>,
    },
    IsStreamsEmpty {
        reply: oneshot::TxOneshot<bool>,
    },
    HasStream {
        live_id: String,
        reply: oneshot::TxOneshot<bool>,
    },
    GetStreamInfo {
        live_id: String,
        reply: oneshot::TxOneshot<Option<Arc<StreamInfo>>>,
    },
    GetRtmpTx {
        live_id: String,
        reply: oneshot::TxOneshot<Option<MTx<mpsc::List<RtmpTag>>>>,
    },
}
