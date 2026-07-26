//! livestream-pipeline: PipelineGraph, PipelineBuilder, engine, Processors, Sinks.
//!
//! Phase 4 — This crate builds on `livestream-core` traits to provide:
//! - `PipelineGraph` + `PipelineBuilder<Current>` type-state API
//! - Pipeline execution engine with task layout and pad backend assignment
//! - `FlvBroadcast` trait (breaks circular dep with transport)
//! - Processors: OTelProbe, SeqCacheProbe, FlvMux, HlsSegmenter, Transcode (stub)
//! - Sinks: FlvSink, MinIoSink
//! - PipelineFactory convenience wiring

pub mod broadcast;
pub mod engine;
pub mod factory;
pub mod graph;
pub mod processor;
pub mod sink;
pub mod task;
