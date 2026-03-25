//! Core SRT stream pulling and segmentation logic.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use crossfire::{MTx, mpsc};
use retry::OperationResult;
use retry::delay::{Exponential, jitter};
use tracing::{debug, error, info, instrument};

use crate::ingest::session::lifecycle::WorkerLifecycle;
use crate::ingest::session::{StreamAdapter, WorkerContext};
use crate::ingest::{StreamInfo, StreamInputOptions};
use crate::media::context::{Context, InputContext};
use crate::media::output::{FlvOutputContext, FlvPacket, HlsOutputContext};
use crate::media::packet::{Packet, PacketReadResult};
use crate::telemetry::metrics;

/// SRT-side adapter that orchestrates one worker run entrypoint.
///
/// Responsibilities:
/// - Bridge async session control with blocking SRT ingest execution.
/// - Delegate packet pulling/segment writing to `BlockingWorker`.
///
/// Out of scope:
/// - No global stream registry/state ownership.
/// - No callback side-effect handling outside lifecycle events.
#[derive(Debug, Default)]
pub struct SrtAdapter {}

impl SrtAdapter {
    pub fn new() -> Self {
        Self {}
    }
}

/// Read outcome abstraction for one packet pull attempt.
///
/// Responsibilities:
/// - Normalize packet-read results for retry/loop control.
///
/// Out of scope:
/// - No error policy or metrics reporting itself.
enum ReadResult {
    /// Packet read was successful and processing can continue.
    Ok,
    /// Reached the End of File (or end of stream), signifying transmission has finished.
    Eof,
}

/// Blocking ingest worker that owns FFmpeg contexts within a single retry cycle.
///
/// Responsibilities:
/// - Pull packets from SRT input and write FLV/HLS outputs.
/// - Emit stream and segment lifecycle signals through `WorkerLifecycle`.
/// - Maintain per-run segmentation cursors (`segment_id`, `last_start_pts`).
///
/// Out of scope:
/// - No manager-level start/stop command handling.
/// - No global resource allocation decisions.
struct BlockingWorker {
    stream_info: Arc<StreamInfo>,
    flv_packet_tx: MTx<mpsc::List<FlvPacket>>,
    lifecycle: WorkerLifecycle,
    stop_signal: Arc<AtomicBool>,

    input_ctx: Option<InputContext>,
    flv_output: Option<FlvOutputContext>,
    hls_output: Option<HlsOutputContext>,

    segment_id: u64,
    last_start_pts: i64,
}

impl BlockingWorker {
    pub fn new(
        stream_info: Arc<StreamInfo>,
        flv_packet_tx: MTx<mpsc::List<FlvPacket>>,
        lifecycle: WorkerLifecycle,
        stop_signal: Arc<AtomicBool>,
    ) -> Self {
        Self {
            stream_info,
            flv_packet_tx,
            lifecycle,
            stop_signal,
            input_ctx: None,
            flv_output: None,
            hls_output: None,
            segment_id: 1,
            last_start_pts: 0,
        }
    }

    fn input_ctx(&self) -> Result<&InputContext> {
        self.input_ctx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Input context not initialized"))
    }

    fn flv_output(&self) -> Result<&FlvOutputContext> {
        self.flv_output
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("FLV output context not initialized"))
    }

    fn hls_output(&self) -> Result<&HlsOutputContext> {
        self.hls_output
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("HLS output context not initialized"))
    }

    fn read_packet_with_retry(&self, packet: &Packet) -> Result<ReadResult> {
        let input_ctx = self.input_ctx()?;

        let result = retry::retry(
            Exponential::from_millis(10).map(jitter).take(5),
            || match packet.read(input_ctx) {
                PacketReadResult::Data => OperationResult::Ok(ReadResult::Ok),
                PacketReadResult::Eof => OperationResult::Ok(ReadResult::Eof),
                PacketReadResult::Retryable { code, message } => {
                    debug!(
                        code = code,
                        message = %message,
                        "Retryable error reading packet, will retry"
                    );
                    OperationResult::Retry(anyhow::anyhow!(
                        "Retryable error reading packet: code={}, message={}",
                        code,
                        message
                    ))
                }
                PacketReadResult::Fatal { code, message } => OperationResult::Err(anyhow::anyhow!(
                    "Fatal error reading packet: code={}, message={}",
                    code,
                    message
                )),
            },
        );

        match result {
            Ok(v) => Ok(v),
            Err(e) => Err(anyhow::anyhow!("Failed to read packet: {}", e)),
        }
    }

    /// Determines if a new segment should be created based on packet and duration.
    fn should_segment(&self, packet: &Packet) -> Option<i64> {
        let input_ctx = self.input_ctx.as_ref()?;

        let current_pts = packet.pts().unwrap_or(0);
        let current_stream = input_ctx.stream(packet.stream_idx())?;
        if !current_stream.is_video_stream() || !packet.is_key_frame() {
            return None;
        }

        if (current_pts - self.last_start_pts) as f64 * current_stream.time_base_f64()
            > self.stream_info.segment_duration() as f64
        {
            return Some(current_pts);
        }

        None
    }

    fn notify_segment_complete(&self) {
        let output_ctx = match self.hls_output() {
            Ok(ctx) => ctx,
            Err(_) => return,
        };

        self.lifecycle.notify_segment_complete(output_ctx.path());
    }

    /// Main loop for pulling stream, segmenting, and writing to disk.
    #[instrument(name = "ingest.srt_worker.loop", skip(self), fields(stream.protocol = "srt", stream.live_id = %self.stream_info.live_id()))]
    fn start_impl(&mut self, input_connected: &mut bool) -> Result<()> {
        let live_id = self.stream_info.live_id().to_string();
        let cache_dir = self.stream_info.cache_dir().to_path_buf();
        let metric_labels = metrics::protocol_labels("srt");

        self.input_ctx = Some(match self.stream_info.input_options() {
            StreamInputOptions::Srt(options) => {
                InputContext::open(options, self.stop_signal.clone())?
            }
            StreamInputOptions::Rtmp { .. } => {
                anyhow::bail!(
                    "RTMP ingest stream '{}' must be handled by server RTMP worker, not FFmpeg SRT worker",
                    live_id
                );
            }
        });

        self.flv_output = Some(FlvOutputContext::create(
            live_id.to_string(),
            self.flv_packet_tx.clone(),
            self.input_ctx()?,
        )?);

        self.hls_output = Some(HlsOutputContext::create_segment(
            &cache_dir,
            self.input_ctx()?,
            self.segment_id,
        )?);

        while !self.stop_signal.load(Ordering::Relaxed) {
            let packet = Packet::alloc()?;
            match self.read_packet_with_retry(&packet) {
                Ok(ReadResult::Ok) => {
                    *input_connected = true;
                    self.lifecycle.notify_stream_started();
                }
                Ok(ReadResult::Eof) => {
                    info!(live_id = %live_id, "End of stream reached");
                    break;
                }
                Err(e) => {
                    error!(error = %e, live_id = %live_id, "Error reading packet, terminating stream");
                    self.lifecycle.notify_stream_stopped(Some(e.to_string()));
                    anyhow::bail!(e);
                }
            }

            metrics::get_metrics().add_network_bytes_in(packet.size() as u64, &metric_labels);
            metrics::get_metrics().add_ingest_packets(1, &metric_labels);

            let cloned_packet = packet.clone();

            if let Some(pts) = self.should_segment(&packet) {
                self.notify_segment_complete();

                self.last_start_pts = pts;
                self.segment_id += 1;

                self.hls_output = Some(HlsOutputContext::create_segment(
                    &cache_dir,
                    self.input_ctx()?,
                    self.segment_id,
                )?);
            }

            packet.rescale_ts_for_ctx(self.input_ctx()?, self.flv_output()?)?;
            if let Err(e) = packet.write(self.flv_output()?) {
                self.notify_segment_complete();
                self.lifecycle
                    .notify_stream_stopped(Some(format!("FLV output write failed: {}", e)));
                anyhow::bail!("Failed to write packet to FLV output: {}", e);
            }

            cloned_packet.rescale_ts_for_ctx(self.input_ctx()?, self.hls_output()?)?;
            if let Err(e) = cloned_packet.write(self.hls_output()?) {
                self.notify_segment_complete();
                self.lifecycle
                    .notify_stream_stopped(Some(format!("HLS output write failed: {}", e)));
                anyhow::bail!("Failed to write packet to TS output: {}", e);
            }
        }

        self.notify_segment_complete();
        self.lifecycle.notify_stream_stopped(None);

        Ok(())
    }
}

impl StreamAdapter for SrtAdapter {
    async fn run(&mut self, ctx: &mut WorkerContext) -> Result<()> {
        let lifecycle = ctx.lifecycle.clone();
        let stream_info = ctx.stream_info.clone();
        let flv_packet_tx = ctx.flv_packet_tx.clone();
        let stop_signal = ctx.stop_signal.clone();

        let lifecycle = tokio::task::spawn_blocking(move || {
            let mut worker = BlockingWorker::new(stream_info, flv_packet_tx, lifecycle, stop_signal);

            let start_span = tracing::info_span!("ingest.srt_worker.start", stream.protocol = "srt", stream.live_id = %worker.stream_info.live_id());

            start_span.in_scope(|| {
                info!(live_id = %worker.stream_info.live_id(), "SRT worker loop starting");
            });

            'worker_loop: loop {
                let mut delay = Exponential::from_millis(10).map(jitter).take(5);

                while let Some(duration) = delay.next() {
                    let mut recovered = false;

                    if let Err(e) = worker.start_impl(&mut recovered) {
                        error!(error = %e, live_id = %worker.stream_info.live_id(), "Error in SRT worker loop");
                        worker.lifecycle.notify_stream_restarting(e.to_string());
                    } else {
                        break 'worker_loop;
                    }

                    std::thread::sleep(duration);

                    if recovered {
                        delay = Exponential::from_millis(10).map(jitter).take(5);
                        continue 'worker_loop;
                    }
                }
            }

            info!(live_id = %worker.stream_info.live_id(), "SRT worker loop exited");
            Ok::<WorkerLifecycle, anyhow::Error>(worker.lifecycle)
        }).await??;

        ctx.lifecycle = lifecycle;
        Ok(())
    }
}
