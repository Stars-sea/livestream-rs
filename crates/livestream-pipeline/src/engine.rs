//! Pipeline execution engine.
//!
//! `PipelineImpl` owns the tokio tasks spawned during `PipelineGraph::build()`
//! and implements the `Pipeline` trait (run, shutdown, handle).

use anyhow::Result;
use livestream_core::traits::{Pipeline, PipelineHandle, PipelineState};
use tokio::task::JoinHandle;

use crate::graph::PipelineGraph;

/// Concrete `Pipeline` implementation.  Created by `PipelineGraph::build()`.
// Fields populated in Phase 4.1 when the engine spawns processor/sink tasks.
#[allow(dead_code)]
pub struct PipelineImpl {
    handle: PipelineHandle,
    tasks: Vec<JoinHandle<()>>,
}

impl PipelineImpl {
    pub(crate) fn from_graph(
        _graph: PipelineGraph,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<Self> {
        Ok(Self {
            handle: PipelineHandle::new(cancel),
            tasks: Vec::new(),
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
        self.handle.set_state(PipelineState::Draining);
        self.handle.cancel_token().cancel();
        self.handle.set_state(PipelineState::Terminated);
        Ok(())
    }

    fn handle(&self) -> PipelineHandle {
        self.handle.clone()
    }
}
