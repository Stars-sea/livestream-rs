use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use anyhow::Result;
use tokio_util::sync::CancellationToken;

/// An executable pipeline.
///
/// Implemented in `livestream-pipeline` (Phase 4). This trait is defined
/// in `livestream-core` so transport can reference it without depending
/// on the pipeline implementation.
pub trait Pipeline: Send + Sync {
    fn run(&self) -> impl Future<Output = Result<()>> + Send;
    fn shutdown(&self) -> impl Future<Output = Result<()>> + Send;
    fn handle(&self) -> PipelineHandle;
}

/// A handle to a running pipeline for external monitoring and control.
#[derive(Clone)]
pub struct PipelineHandle {
    state: Arc<AtomicU8>,
    cancel: CancellationToken,
}

impl PipelineHandle {
    pub fn new(cancel: CancellationToken) -> Self {
        Self {
            state: Arc::new(AtomicU8::new(PipelineState::Initializing as u8)),
            cancel,
        }
    }

    pub fn state(&self) -> PipelineState {
        match self.state.load(Ordering::Acquire) {
            0 => PipelineState::Initializing,
            1 => PipelineState::Running,
            2 => PipelineState::Draining,
            3 => PipelineState::Terminated,
            _ => unreachable!("invalid PipelineState discriminant"),
        }
    }

    #[allow(dead_code)] // used by Pipeline impl in Phase 4
    pub(crate) fn set_state(&self, state: PipelineState) {
        self.state.store(state as u8, Ordering::Release);
    }

    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipelineState {
    Initializing = 0,
    Running = 1,
    Draining = 2,
    Terminated = 3,
}
