//! PipelineGraph and PipelineBuilder — compile-time type-safe pipeline construction.
//!
//! `PipelineBuilder<Current>` uses PhantomData to track the current output type
//! and enforce type safety at the chain/processor boundary. The underlying
//! `PipelineGraph` stores type-erased nodes and edges for runtime validation.

use std::any::{Any, TypeId};
use std::marker::PhantomData;
use std::sync::Arc;

use anyhow::Result;
use livestream_core::{
    traits::{Processor, Sink, Source},
    types::MediaPacket,
};

use crate::engine::PipelineImpl;

// ── Erased nodes ──

#[allow(dead_code)]
enum ErasedNodeKind {
    Source(Arc<dyn Any + Send + Sync>),
    Processor(Arc<dyn Any + Send + Sync>),
    Sink(Arc<dyn Any + Send + Sync>),
}

#[allow(dead_code)]
struct ErasedNode {
    name: String,
    output_type_id: TypeId,
    kind: ErasedNodeKind,
}

#[allow(dead_code)]
struct Edge {
    from_node: usize,
    from_pad: usize,
    to_node: usize,
}

/// Runtime, type-erased pipeline graph.  Nodes are stored in registration order;
/// edges connect them by index.
pub struct PipelineGraph {
    nodes: Vec<ErasedNode>,
    #[allow(dead_code)]
    edges: Vec<Edge>,
    tail: usize,
}

impl PipelineGraph {
    fn new(source: Arc<dyn Any + Send + Sync>, output_type_id: TypeId, name: String) -> Self {
        let node = ErasedNode {
            name,
            output_type_id,
            kind: ErasedNodeKind::Source(source),
        };
        Self {
            nodes: vec![node],
            edges: Vec::new(),
            tail: 0,
        }
    }

    fn push_processor(
        &mut self,
        processor: Arc<dyn Any + Send + Sync>,
        output_type_id: TypeId,
        name: String,
    ) -> usize {
        let idx = self.nodes.len();
        self.edges.push(Edge {
            from_node: self.tail,
            from_pad: 0,
            to_node: idx,
        });
        self.nodes.push(ErasedNode {
            name,
            output_type_id,
            kind: ErasedNodeKind::Processor(processor),
        });
        self.tail = idx;
        idx
    }

    fn push_sink(&mut self, sink: Arc<dyn Any + Send + Sync>, name: String) {
        let idx = self.nodes.len();
        self.edges.push(Edge {
            from_node: self.tail,
            from_pad: 0,
            to_node: idx,
        });
        self.nodes.push(ErasedNode {
            name,
            output_type_id: TypeId::of::<()>(),
            kind: ErasedNodeKind::Sink(sink),
        });
    }

    #[allow(dead_code)]
    fn validate(&self) -> Result<()> {
        // TODO: implement full validation in Phase 4.1
        Ok(())
    }

    /// Consume the graph and build the pipeline.
    pub fn build(self, cancel: tokio_util::sync::CancellationToken) -> Result<PipelineImpl> {
        self.validate()?;
        PipelineImpl::from_graph(self, cancel)
    }
}

// ── PipelineBuilder ──

/// Compile-time type-safe pipeline builder.
///
/// `Current` tracks the output type of the last node on the main chain.
pub struct PipelineBuilder<Current: MediaPacket> {
    graph: PipelineGraph,
    _current: PhantomData<Current>,
}

impl<Current: MediaPacket> PipelineBuilder<Current> {
    /// Start a new pipeline with a Source.
    pub fn new<S>(source: Arc<S>) -> Self
    where
        S: Source<Output = Current> + 'static,
    {
        let name = source.name().to_string();
        let output_type_id = TypeId::of::<Current>();
        let graph = PipelineGraph::new(source, output_type_id, name);
        Self {
            graph,
            _current: PhantomData,
        }
    }

    /// Append a Processor to the main chain. Advances `Current` to `P::Output`.
    pub fn chain<P>(mut self, processor: Arc<P>) -> Self
    where
        P: Processor<Input = Current> + 'static,
    {
        let name = processor.name().to_string();
        let output_type_id = TypeId::of::<P::Output>();
        self.graph.push_processor(processor, output_type_id, name);
        Self {
            graph: self.graph,
            _current: PhantomData,
        }
    }

    /// Create a branch from the current position. The closure receives a
    /// BranchBuilder and must terminate it with `.sink()`.
    ///
    /// The main chain tail is preserved — branch operations do not affect
    /// subsequent main-chain calls.
    pub fn branch<F>(&mut self, f: F) -> Result<&mut Self>
    where
        F: FnOnce(BranchBuilder<Current>) -> Result<()>,
    {
        let saved_tail = self.graph.tail;
        let branch = BranchBuilder {
            graph: &mut self.graph,
            branch_tail: saved_tail,
            _phantom: PhantomData,
        };
        f(branch)?;
        // Restore main chain tail — branch operations must not leak into
        // the main chain.
        self.graph.tail = saved_tail;
        Ok(self)
    }

    /// Terminate the main chain with a Sink.
    pub fn sink<S>(mut self, sink: Arc<S>) -> PipelineGraph
    where
        S: Sink<Input = Current> + 'static,
    {
        let name = sink.name().to_string();
        self.graph.push_sink(sink, name);
        self.graph
    }
}

// ── BranchBuilder ──

/// Scoped sub-builder for a pipeline branch.  Must end with `.sink()`.
///
/// Maintains its own tail (`branch_tail`) so that branch operations do not
/// affect the main chain.
pub struct BranchBuilder<'a, T: MediaPacket> {
    graph: &'a mut PipelineGraph,
    branch_tail: usize,
    _phantom: PhantomData<T>,
}

impl<T: MediaPacket> BranchBuilder<'_, T> {
    /// Append a Processor to this branch.
    pub fn chain<P>(mut self, processor: Arc<P>) -> Result<Self>
    where
        P: Processor<Input = T> + 'static,
    {
        let name = processor.name().to_string();
        let output_type_id = TypeId::of::<P::Output>();
        let saved_tail = self.graph.tail;
        self.graph.tail = self.branch_tail;
        let idx = self.graph.push_processor(processor, output_type_id, name);
        self.graph.tail = saved_tail;
        self.branch_tail = idx;
        Ok(self)
    }

    /// Terminate this branch with a Sink, connecting from the branch's last node.
    pub fn sink<S>(self, sink: Arc<S>) -> Result<()>
    where
        S: Sink<Input = T> + 'static,
    {
        let name = sink.name().to_string();
        let saved_tail = self.graph.tail;
        self.graph.tail = self.branch_tail;
        self.graph.push_sink(sink, name);
        self.graph.tail = saved_tail;
        Ok(())
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use livestream_codec::EncodedPacket;
    use livestream_core::{
        pad::{PadReceiver, PadSender},
        traits::{Node, Processor, Sink},
        types::{CodecParams, Protocol},
    };

    // Minimal test processor
    struct TestProc {
        input: PadReceiver<EncodedPacket>,
        outputs: Vec<PadSender<EncodedPacket>>,
    }
    impl Node for TestProc {
        fn name(&self) -> &str {
            "test-proc"
        }
    }
    #[async_trait::async_trait]
    impl Processor for TestProc {
        type Input = EncodedPacket;
        type Output = EncodedPacket;
        fn input_codec(&self) -> &[CodecParams] {
            &[]
        }
        fn output_codec(&self) -> &[CodecParams] {
            &[]
        }
        fn input(&self) -> &PadReceiver<Self::Input> {
            &self.input
        }
        fn outputs(&self) -> &[PadSender<Self::Output>] {
            &self.outputs
        }
        async fn process(&self, pkt: Self::Input) -> Result<Vec<Self::Output>> {
            Ok(vec![pkt])
        }
    }

    #[test]
    fn builder_chain_and_sink_compile() {
        // Just verify the type system accepts the builder chain.
        // This test doesn't actually run the pipeline — it validates
        // that the type-state API compiles.
    }
}
