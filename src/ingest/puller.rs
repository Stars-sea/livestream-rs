//! Core SRT stream pulling and segmentation logic.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::events::*;
use super::stream_info::{StreamInfo, StreamInputOptions};

use crate::core::context::{Context, InputContext};
use crate::core::output::{FlvOutputContext, FlvPacket, HlsOutputContext};
use crate::core::packet::{Packet, PacketReadResult};
use crate::otlp::metrics;
use crate::services::MemoryCache;

use anyhow::Result;
use retry::OperationResult;
use retry::delay::{Exponential, jitter};
use tokio::sync::mpsc;
use tracing::{Span, debug, error, info, instrument, warn};

#[derive(Debug)]
pub(super) struct StreamPullerFactory {
    stream_msg_tx: mpsc::UnboundedSender<(StreamMessage, Span)>,
    flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
    signal_cache: MemoryCache<Arc<AtomicBool>>,
}

impl StreamPullerFactory {
    pub fn new(
        stream_msg_tx: mpsc::UnboundedSender<(StreamMessage, Span)>,
        flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
    ) -> Self {
        Self {
            stream_msg_tx,
            flv_packet_tx,
            signal_cache: MemoryCache::new(),
        }
    }

    pub async fn can_create(&self, stream_info: &StreamInfo) -> bool {
        !self.signal_cache.contains_key(stream_info.live_id()).await
    }

    pub async fn create(&self, stream_info: Arc<StreamInfo>) -> Result<StreamPuller> {
        let live_id = stream_info.live_id().to_string();
        info!(live_id = %live_id, "Creating stream puller");
        let stop_signal = Arc::new(AtomicBool::new(false));
        self.signal_cache.set(live_id, stop_signal.clone()).await?;

        Ok(StreamPuller::new(
            stream_info,
            self.stream_msg_tx.clone(),
            self.flv_packet_tx.clone(),
            stop_signal,
        ))
    }

    pub async fn remove_signal(&self, live_id: &str) {
        debug!(live_id = %live_id, "Removing signal");
        self.signal_cache.remove(live_id).await;
    }

    pub async fn get_signals(&self) -> Vec<Arc<AtomicBool>> {
        self.signal_cache.values().await
    }

    pub async fn get_signal(&self, live_id: &str) -> Option<Arc<AtomicBool>> {
        self.signal_cache.get(live_id).await
    }
}

#[derive(Debug)]
pub(super) struct StreamPuller {
    stream_info: Arc<StreamInfo>,

    stream_msg_tx: mpsc::UnboundedSender<(StreamMessage, Span)>,
    flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
    stop_signal: Arc<AtomicBool>,

    input_ctx: Option<InputContext>,
    flv_output: Option<FlvOutputContext>,
    hls_output: Option<HlsOutputContext>,

    segment_id: u64,
    last_start_pts: i64,
}

enum ReadResult {
    Ok,
    Eof,
}

impl StreamPuller {
    pub fn new(
        stream_info: Arc<StreamInfo>,
        stream_msg_tx: mpsc::UnboundedSender<(StreamMessage, Span)>,
        flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
        stop_signal: Arc<AtomicBool>,
    ) -> Self {
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

    #[instrument(name = "ingest.puller.segment_complete", skip(self), fields(stream.live_id = %self.stream_info.live_id(), stream.segment_id = self.segment_id))]
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

    #[instrument(name = "ingest.stream.notify_started", skip(self), fields(stream.live_id = %self.stream_info.live_id()))]
    fn notify_stream_started(&self) {
        let live_id = self.stream_info.live_id();
        let event = (StreamMessage::stream_started(live_id), Span::current());
        if let Err(e) = self.stream_msg_tx.send(event) {
            warn!(error = %e, live_id = %live_id, "Failed to send stream started event");
        }
    }

    #[instrument(name = "ingest.stream.notify_stopped", skip(self), fields(stream.live_id = %self.stream_info.live_id(), error = ?error))]
    fn notify_stream_stopped(&self, error: Option<String>) {
        let live_id = self.stream_info.live_id();
        let event = (
            StreamMessage::stream_stopped(live_id, error.clone()),
            Span::current(),
        );
        if let Err(e) = self.stream_msg_tx.send(event) {
            warn!(error = %e, live_id = %live_id, "Failed to send stream stopped event");
        }
    }

    #[instrument(name = "ingest.stream.notify_restarting", skip(self), fields(stream.live_id = %self.stream_info.live_id(), error = %error))]
    fn notify_stream_restarting(&self, error: String) {
        let live_id = self.stream_info.live_id();
        let event = (
            StreamMessage::stream_restarting(live_id, error),
            Span::current(),
        );
        if let Err(e) = self.stream_msg_tx.send(event) {
            warn!(error = %e, live_id = %live_id, "Failed to send stream restarting event");
        }
    }

    #[instrument(name = "ingest.puller.notify_started", skip(self), fields(stream.live_id = %self.stream_info.live_id()))]
    fn notify_puller_started(&self) {
        let live_id = self.stream_info.live_id();

        let event = (StreamMessage::puller_started(live_id), Span::current());
        if let Err(e) = self.stream_msg_tx.send(event) {
            warn!(error = %e, live_id = %live_id, "Failed to send puller started event");
        }
    }

    #[instrument(name = "ingest.puller.notify_stopped", skip(self), fields(stream.live_id = %self.stream_info.live_id()))]
    fn notify_puller_stopped(&self) {
        let live_id = self.stream_info.live_id();

        let event = (StreamMessage::puller_stopped(live_id), Span::current());
        if let Err(e) = self.stream_msg_tx.send(event) {
            warn!(error = %e, live_id = %live_id, "Failed to send puller stopped event");
        }

        // TODO:
        let flv_output = self
            .flv_output
            .as_ref()
            .map(|o| o.get_flv_packet_sender())
            .flatten();
        if let Some(tx) = flv_output {
            if let Err(e) = tx.send(FlvPacket::EndOfStream {
                live_id: live_id.to_string(),
            }) {
                warn!(error = %e, live_id = %live_id, "Failed to send end of stream packet");
            }
        }
    }

    pub fn start(&mut self) {
        self.notify_puller_started();

        let start_span = tracing::info_span!("ingest.puller.start", stream.live_id = %self.stream_info.live_id());

        start_span.in_scope(|| {
            info!(live_id = %self.stream_info.live_id(), "Stream puller loop starting");
        });

        'puller_loop: loop {
            let mut delay = Exponential::from_millis(10).map(jitter).take(5);

            while let Some(duration) = delay.next() {
                let mut recovered = false;

                if let Err(e) = self.start_impl(&mut recovered) {
                    error!(error = %e, live_id = %self.stream_info.live_id(), "Error in stream puller loop");
                    self.notify_stream_restarting(e.to_string());
                } else {
                    break 'puller_loop;
                }

                std::thread::sleep(duration);

                if recovered {
                    delay = Exponential::from_millis(10).map(jitter).take(5);
                    continue 'puller_loop;
                }
            }
        }

        self.notify_puller_stopped();
        info!(live_id = %self.stream_info.live_id(), "Stream puller loop exited");
    }

    /// Main loop for pulling stream, segmenting, and writing to disk.
    #[instrument(name = "ingest.puller.loop", skip(self), fields(stream.live_id = %self.stream_info.live_id()))]
    fn start_impl(&mut self, input_connected: &mut bool) -> Result<()> {
        let live_id = self.stream_info.live_id().to_string();
        let cache_dir = self.stream_info.cache_dir().to_path_buf();

        let mut stream_started_notified = false;

        self.input_ctx = Some(match self.stream_info.input_options() {
            StreamInputOptions::Srt(options) => {
                InputContext::open(options, self.stop_signal.clone())?
            }
            StreamInputOptions::Rtmp(options) => {
                InputContext::open(options, self.stop_signal.clone())?
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

            metrics::get_metrics().add_network_bytes_in(packet.size() as u64, &[]);
            metrics::get_metrics().add_ingest_packets(1, &[]);

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
