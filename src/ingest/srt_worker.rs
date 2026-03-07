//! Core SRT stream pulling and segmentation logic.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::events::*;
use super::lifecycle;
use super::stream_info::{StreamInfo, StreamInputOptions};

use crate::core::context::{Context, InputContext};
use crate::core::output::{FlvOutputContext, FlvPacket, HlsOutputContext};
use crate::core::packet::{Packet, PacketReadResult};
use crate::otlp::metrics;

use anyhow::Result;
use retry::OperationResult;
use retry::delay::{Exponential, jitter};
use tokio::sync::mpsc;
use tracing::{Span, debug, error, info, instrument, warn};

#[derive(Debug)]
pub(super) struct SrtWorker {
    stream_info: Arc<StreamInfo>,

    stream_msg_tx: mpsc::UnboundedSender<(StreamMessage, Span)>,
    flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
    stop_signal: Arc<AtomicBool>,

    input_ctx: Option<InputContext>,
    flv_output: Option<FlvOutputContext>,
    hls_output: Option<HlsOutputContext>,

    segment_id: u64,
    last_start_pts: i64,
    lifecycle: lifecycle::WorkerLifecycle,
}

enum ReadResult {
    Ok,
    Eof,
}

impl SrtWorker {
    pub fn new(
        stream_info: Arc<StreamInfo>,
        stream_msg_tx: mpsc::UnboundedSender<(StreamMessage, Span)>,
        flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
        stop_signal: Arc<AtomicBool>,
    ) -> Self {
        let lifecycle = lifecycle::WorkerLifecycle::new(stream_info.live_id());
        Self {
            stream_info,
            stream_msg_tx,
            flv_packet_tx,
            stop_signal,
            input_ctx: None,
            flv_output: None,
            hls_output: None,
            segment_id: 1,
            last_start_pts: 0,
            lifecycle,
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

    #[instrument(name = "ingest.srt_worker.segment_complete", skip(self), fields(stream.protocol = "srt", stream.live_id = %self.stream_info.live_id(), stream.segment_id = self.segment_id))]
    fn notify_segment_complete(&self) {
        let output_ctx = match self.hls_output() {
            Ok(ctx) => ctx,
            Err(_) => return,
        };

        let live_id = self.stream_info.live_id();
        let event = (
            StreamMessage::segment_complete(live_id, output_ctx.path()),
            Span::current(),
        );
        if let Err(e) = self.stream_msg_tx.send(event) {
            warn!(error = %e, live_id = %live_id, "Failed to send final segment complete event");
        }
    }

    #[instrument(name = "ingest.srt_worker.stream.notify_started", skip(self), fields(stream.protocol = "srt", stream.live_id = %self.stream_info.live_id()))]
    fn notify_stream_started(&mut self) {
        self.lifecycle.notify_stream_started(&self.stream_msg_tx);
    }

    #[instrument(name = "ingest.srt_worker.stream.notify_stopped", skip(self), fields(stream.protocol = "srt", stream.live_id = %self.stream_info.live_id(), error = ?error))]
    fn notify_stream_stopped(&mut self, error: Option<String>) {
        self.lifecycle
            .notify_stream_stopped(&self.stream_msg_tx, error);
    }

    #[instrument(name = "ingest.srt_worker.stream.notify_restarting", skip(self), fields(stream.protocol = "srt", stream.live_id = %self.stream_info.live_id(), error = %error))]
    fn notify_stream_restarting(&mut self, error: String) {
        self.lifecycle
            .notify_stream_restarting(&self.stream_msg_tx, error);
    }

    #[instrument(name = "ingest.srt_worker.notify_started", skip(self), fields(stream.protocol = "srt", stream.live_id = %self.stream_info.live_id()))]
    fn notify_ingest_worker_started(&mut self) {
        self.lifecycle
            .notify_ingest_worker_started(&self.stream_msg_tx);
    }

    #[instrument(name = "ingest.srt_worker.notify_stopped", skip(self), fields(stream.protocol = "srt", stream.live_id = %self.stream_info.live_id()))]
    fn finalize_lifecycle(&mut self, error: Option<String>) {
        let eos_tx = self
            .flv_output
            .as_ref()
            .and_then(|output| output.get_flv_packet_sender())
            .unwrap_or_else(|| self.flv_packet_tx.clone());

        self.lifecycle
            .notify_stream_stopped(&self.stream_msg_tx, error);
        self.lifecycle.send_end_of_stream_once(&eos_tx);
        self.lifecycle
            .notify_ingest_worker_stopped(&self.stream_msg_tx);
    }

    pub fn start(&mut self) {
        self.notify_ingest_worker_started();

        let start_span = tracing::info_span!("ingest.srt_worker.start", stream.protocol = "srt", stream.live_id = %self.stream_info.live_id());

        start_span.in_scope(|| {
            info!(live_id = %self.stream_info.live_id(), "SRT worker loop starting");
        });

        'worker_loop: loop {
            let mut delay = Exponential::from_millis(10).map(jitter).take(5);

            while let Some(duration) = delay.next() {
                let mut recovered = false;

                if let Err(e) = self.start_impl(&mut recovered) {
                    error!(error = %e, live_id = %self.stream_info.live_id(), "Error in SRT worker loop");
                    self.notify_stream_restarting(e.to_string());
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

        self.finalize_lifecycle(None);
        info!(live_id = %self.stream_info.live_id(), "SRT worker loop exited");
    }

    /// Main loop for pulling stream, segmenting, and writing to disk.
    #[instrument(name = "ingest.srt_worker.loop", skip(self), fields(stream.protocol = "srt", stream.live_id = %self.stream_info.live_id()))]
    fn start_impl(&mut self, input_connected: &mut bool) -> Result<()> {
        let live_id = self.stream_info.live_id().to_string();
        let cache_dir = self.stream_info.cache_dir().to_path_buf();
        let metric_labels = metrics::protocol_labels("srt");

        let mut stream_started_notified = false;

        self.input_ctx = Some(match self.stream_info.input_options() {
            StreamInputOptions::Srt(options) => {
                InputContext::open(options, self.stop_signal.clone())?
            }
            StreamInputOptions::Rtmp(_) => {
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
                Ok(ReadResult::Ok) => *input_connected = true,
                Ok(ReadResult::Eof) => {
                    info!(live_id = %live_id, "End of stream reached");
                    break;
                }
                Err(e) => {
                    error!(error = %e, live_id = %live_id, "Error reading packet, terminating stream");
                    anyhow::bail!(e);
                }
            }

            metrics::get_metrics().add_network_bytes_in(packet.size() as u64, &metric_labels);
            metrics::get_metrics().add_ingest_packets(1, &metric_labels);

            if !stream_started_notified {
                self.notify_stream_started();
                stream_started_notified = true;
            }

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
                self.notify_stream_stopped(Some(format!("FLV output write failed: {}", e)));
                anyhow::bail!("Failed to write packet to FLV output: {}", e);
            }

            cloned_packet.rescale_ts_for_ctx(self.input_ctx()?, self.hls_output()?)?;
            if let Err(e) = cloned_packet.write(self.hls_output()?) {
                self.notify_segment_complete();
                self.notify_stream_stopped(Some(format!("HLS output write failed: {}", e)));
                anyhow::bail!("Failed to write packet to TS output: {}", e);
            }
        }

        self.notify_segment_complete();
        self.notify_stream_stopped(None);

        Ok(())
    }
}

