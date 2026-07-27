//! Pipeline execution engine.
//!
//! `PipelineImpl` owns the tokio tasks spawned during pipeline construction
//! and implements the `Pipeline` trait (run, shutdown, handle).

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use livestream_core::traits::{Pipeline, PipelineHandle, PipelineState};
use parking_lot::Mutex;
use tokio::task::JoinHandle;

/// Concrete `Pipeline` implementation.
pub struct PipelineImpl {
    handle: PipelineHandle,
    /// Task handles behind a shareable Mutex so deferred init (HLS) can
    /// push spawned handles without fire-and-forget.
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl PipelineImpl {
    /// Construct a PipelineImpl directly from a handle and spawned task handles.
    pub fn new(handle: PipelineHandle, tasks: Vec<JoinHandle<()>>) -> Self {
        Self {
            handle,
            tasks: Arc::new(Mutex::new(tasks)),
        }
    }

    /// Construct a PipelineImpl with a pre-existing shared task list.
    /// Used when deferred init (HLS) needs write access to the task list
    /// before the PipelineImpl is fully constructed.
    pub fn with_shared_tasks(
        handle: PipelineHandle,
        tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    ) -> Self {
        Self { handle, tasks }
    }

    /// Return a clone of the shared task list for deferred init (HLS).
    pub fn tasks_arc(&self) -> Arc<Mutex<Vec<JoinHandle<()>>>> {
        Arc::clone(&self.tasks)
    }

    /// Append task handles after construction.  Used by deferred HLS init
    /// which constructs and spawns pipeline branches asynchronously.
    pub fn push_tasks(&self, handles: Vec<JoinHandle<()>>) {
        self.tasks.lock().extend(handles);
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
