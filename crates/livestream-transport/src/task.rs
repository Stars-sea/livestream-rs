//! Generic async task loops for processor and sink execution.

use std::sync::Arc;

use livestream_core::traits::{Processor, Sink};
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

                match processor.process(pkt).await {
                    Ok(results) => {
                        for item in results {
                            for out_pad in processor.outputs() {
                                if out_pad.send(item.clone()).is_err() {
                                    // Channel closed — stop sending to remaining pads
                                    // but continue to next item (may have different codec pads).
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => tracing::warn!(processor=%processor.name(), error=%e, "drop"),
                }
            }
            _ = cancel.cancelled() => break,
        }
    }
    if let Err(e) = processor.close().await {
        tracing::warn!(processor = %processor.name(), error = %e, "close error");
    }
    tracing::debug!(processor = %processor.name(), "Processor loop ended");
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
                    tracing::warn!(sink=%sink.name(), error=%e, "drop");
                }
            }
            _ = cancel.cancelled() => break,
        }
    }
    tracing::debug!(sink = %sink.name(), "Sink loop ended");
}
