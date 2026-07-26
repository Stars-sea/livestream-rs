//! Pipeline execution engine.
//!
//! `PipelineImpl` owns the tokio tasks spawned during pipeline construction
//! and implements the `Pipeline` trait (run, shutdown, handle).

use parking_lot::Mutex;
use std::time::Duration;

use anyhow::Result;
use livestream_core::traits::{Pipeline, PipelineHandle, PipelineState};
use tokio::task::JoinHandle;

use crate::graph::PipelineGraph;

/// Concrete `Pipeline` implementation.
pub struct PipelineImpl {
    handle: PipelineHandle,
    /// Task handles are behind a Mutex so `shutdown(&self)` can take them.
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl PipelineImpl {
    /// Construct a PipelineImpl directly from a handle and spawned task handles.
    pub fn new(handle: PipelineHandle, tasks: Vec<JoinHandle<()>>) -> Self {
        Self {
            handle,
            tasks: Mutex::new(tasks),
        }
    }

    pub(crate) fn from_graph(
        _graph: PipelineGraph,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<Self> {
        Ok(Self {
            handle: PipelineHandle::new(cancel),
            tasks: Mutex::new(Vec::new()),
        })
    }
}

impl Pipeline for PipelineImpl {
    async fn run(&self) -> Result<()> {
        self.handle.set_state(PipelineState::Running);
        let cancel = self.handle.cancel_token();
        cancel.cancelled().await;
        self.shutdown().await
    }

    async fn shutdown(&self) -> Result<()> {
        const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

        self.handle.set_state(PipelineState::Draining);

        // 1. Signal all tasks to stop.
        self.handle.cancel_token().cancel();

        // 2. Take ownership of task handles and wait for them to drain.
        let tasks: Vec<JoinHandle<()>> = std::mem::take(&mut *self.tasks.lock());

        if tasks.is_empty() {
            self.handle.set_state(PipelineState::Terminated);
            return Ok(());
        }

        let task_count = tasks.len();
        let drain = drain_all(tasks);
        match tokio::time::timeout(SHUTDOWN_TIMEOUT, drain).await {
            Ok(()) => {
                tracing::info!(task_count, "All pipeline tasks drained successfully");
            }
            Err(_) => {
                tracing::warn!(
                    task_count,
                    timeout_secs = SHUTDOWN_TIMEOUT.as_secs(),
                    "Pipeline shutdown timed out — some tasks may have been aborted"
                );
            }
        }

        self.handle.set_state(PipelineState::Terminated);
        Ok(())
    }

    fn handle(&self) -> PipelineHandle {
        self.handle.clone()
    }
}

/// Wait for all task handles to complete, logging panics.
async fn drain_all(tasks: Vec<JoinHandle<()>>) {
    for (i, task) in tasks.into_iter().enumerate() {
        if let Err(e) = task.await {
            tracing::warn!(
                task_index = i,
                error = %e,
                "Pipeline task panicked during shutdown"
            );
        }
    }
}
