//! Pipeline execution engine.
//!
//! `PipelineImpl` owns the tokio tasks spawned during pipeline construction
//! and implements the `Pipeline` trait (run, shutdown, handle).

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use livestream_core::traits::{Pipeline, PipelineHandle, PipelineState};
use livestream_telemetry::{metric_pipeline_stream_ended, metric_pipeline_stream_started};
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
        metric_pipeline_stream_started!();
        Self {
            handle,
            tasks: Arc::new(Mutex::new(tasks)),
        }
    }

    /// Construct a PipelineImpl with a pre-existing shared task list.
    /// Used when deferred init (HLS) needs write access to the task list
    pub fn with_shared_tasks(
        handle: PipelineHandle,
        tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    ) -> Self {
        metric_pipeline_stream_started!();
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
            metric_pipeline_stream_ended!();
            self.handle.set_state(PipelineState::Terminated);
            return Ok(());
        }

        let task_count = tasks.len();
        let (finished, aborted) = drain_all(tasks, SHUTDOWN_TIMEOUT).await;
        if aborted > 0 {
            tracing::warn!(
                task_count,
                finished,
                aborted,
                timeout_secs = SHUTDOWN_TIMEOUT.as_secs(),
                "{aborted} tasks aborted after drain timeout"
            );
        } else {
            tracing::info!(task_count, "All pipeline tasks drained successfully");
        }

        // 3. Abort any task handles registered after the initial take
        //    (deferred HLS init can push handles while the drain runs).
        let late: Vec<JoinHandle<()>> = std::mem::take(&mut *self.tasks.lock());
        let late_count = late.len();
        for handle in late {
            handle.abort();
        }
        if late_count > 0 {
            tracing::warn!(late_count, "Aborted late-registered tasks during shutdown");
        }

        metric_pipeline_stream_ended!();
        self.handle.set_state(PipelineState::Terminated);
        Ok(())
    }

    fn handle(&self) -> PipelineHandle {
        self.handle.clone()
    }
}

/// Wait for all task handles to complete within `timeout`, logging panics.
/// Tasks that do not finish by the deadline are aborted.
/// Returns `(finished, aborted)` counts.
async fn drain_all(tasks: Vec<JoinHandle<()>>, timeout: Duration) -> (usize, usize) {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut finished = 0usize;
    let mut aborted = 0usize;
    for (i, mut task) in tasks.into_iter().enumerate() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            // Deadline already exceeded; abort instead of waiting further.
            task.abort();
            aborted += 1;
            continue;
        }
        match tokio::time::timeout(remaining, &mut task).await {
            Ok(Ok(())) => finished += 1,
            Ok(Err(e)) => {
                tracing::warn!(
                    task_index = i,
                    error = %e,
                    "Pipeline task panicked during shutdown"
                );
                finished += 1;
            }
            Err(_) => {
                // Timed out waiting for this task: abort it so it cannot
                // linger after shutdown.
                task.abort();
                aborted += 1;
            }
        }
    }
    (finished, aborted)
}
