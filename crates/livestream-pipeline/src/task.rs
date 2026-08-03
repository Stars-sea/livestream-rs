//! Generic async task loops for processor and sink execution.

use std::sync::Arc;

use livestream_core::channel::SendError;
use livestream_core::traits::{Processor, Sink};
use livestream_telemetry::metric_pipeline_error;
use tokio_util::sync::CancellationToken;

/// Run a processor's consume loop in the current task.
pub async fn run_processor<P>(processor: Arc<P>, cancel: CancellationToken)
where
    P: Processor + 'static,
    P::Output: Clone,
{
    tracing::debug!(processor = %processor.name(), "Processor loop started");
    loop {
        // Check demand before pulling to avoid consuming packets
        // that would be dropped when no downstream consumer wants them.
        if !processor.should_process() {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(50)) => {}
            }
            continue;
        }
        tokio::select! {
            pkt = processor.input().recv() => {
                let Some(pkt) = pkt else { break };
                process_one(&processor, pkt).await;
            }
            _ = cancel.cancelled() => break,
        }
    }
    if let Err(e) = processor.close().await {
        tracing::warn!(processor = %processor.name(), error = %e, "close error");
    }
    tracing::debug!(processor = %processor.name(), "Processor loop ended");
}

/// Run one packet through the processor and fan its results out to the
/// output pads. Processing errors are counted and logged, then dropped.
async fn process_one<P>(processor: &Arc<P>, pkt: P::Input)
where
    P: Processor + 'static,
    P::Output: Clone,
{
    match processor.process(pkt).await {
        Ok(results) => fan_out(processor, results),
        Err(e) => {
            metric_pipeline_error!(processor.name().to_string());
            tracing::warn!(processor=%processor.name(), error=%e, "drop");
        }
    }
}

/// Deliver one batch of processor results to every output pad.
///
/// A `Full` pad drops its copy (backpressure) without affecting the other
/// outputs; a `Closed` pad is dead and stops receiving.
fn fan_out<P>(processor: &Arc<P>, results: Vec<P::Output>)
where
    P: Processor + 'static,
    P::Output: Clone,
{
    for item in results {
        for out_pad in processor.outputs() {
            if !deliver_to_pad(processor, out_pad, &item) {
                break;
            }
        }
    }
}

/// Deliver one item to one output pad. Returns `false` when the pad is
/// closed, so the caller can skip the remaining pads for this item.
fn deliver_to_pad<P>(
    processor: &Arc<P>,
    out_pad: &livestream_core::pad::PadSender<P::Output>,
    item: &P::Output,
) -> bool
where
    P: Processor + 'static,
    P::Output: Clone,
{
    match out_pad.send(item.clone()) {
        Ok(()) => true,
        Err(SendError::Full) => {
            // Backpressure: the item is dropped for this output only,
            // the remaining outputs still get their copy.
            tracing::debug!(
                processor = %processor.name(),
                "output pad full, item dropped for this output"
            );
            true
        }
        Err(SendError::Closed) => {
            // Receiver is gone: this output is dead, stop sending to it.
            tracing::debug!(
                processor = %processor.name(),
                "output pad closed, no longer sending"
            );
            false
        }
    }
}

/// Run a sink's consume loop in the current task.
pub async fn run_sink<Si>(sink: Arc<Si>, cancel: CancellationToken)
where
    Si: Sink + 'static,
{
    tracing::debug!(sink = %sink.name(), "Sink loop started");
    loop {
        tokio::select! {
            item = sink.input().recv() => {
                let Some(item) = item else { break };
                if let Err(e) = sink.consume(item).await {
                    metric_pipeline_error!(sink.name().to_string());
                    tracing::warn!(sink=%sink.name(), error=%e, "drop");
                }
            }
            _ = cancel.cancelled() => break,
        }
    }
    tracing::debug!(sink = %sink.name(), "Sink loop ended");
}
