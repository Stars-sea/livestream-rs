//! Core SRT stream pulling and segmentation logic.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::events::*;
use super::stream_info::StreamInfo;

use crate::core::context::Context;
use crate::core::input::SrtInputContext;
use crate::core::output::{FlvOutputContext, FlvPacket, HlsOutputContext};
use crate::core::packet::Packet;
use crate::services::MemoryCache;

use anyhow::Result;
use log::{info, warn};
use tokio::sync::mpsc;

#[derive(Debug)]
pub(super) struct StreamPullerFactory {
    stream_msg_tx: mpsc::UnboundedSender<StreamMessage>,
    flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
    signal_cache: MemoryCache<Arc<AtomicBool>>,
}

impl StreamPullerFactory {
    pub fn new(
        stream_msg_tx: mpsc::UnboundedSender<StreamMessage>,
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

    pub async fn create(&self, stream_info: StreamInfo) -> Result<StreamPuller> {
        let live_id = stream_info.live_id().to_string();
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
        self.signal_cache.remove(live_id).await;
    }

    pub async fn get_signal(&self, live_id: &str) -> Option<Arc<AtomicBool>> {
        self.signal_cache.get(live_id).await
    }

    pub fn create_blocking(&self, stream_info: StreamInfo) -> Result<StreamPuller> {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(self.create(stream_info))
    }
}

#[derive(Debug)]
pub(super) struct StreamPuller {
    stream_info: StreamInfo,

    stream_msg_tx: mpsc::UnboundedSender<StreamMessage>,
    flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
    stop_signal: Arc<AtomicBool>,

    srt_input: Option<SrtInputContext>,
    flv_output: Option<FlvOutputContext>,
    hls_output: Option<HlsOutputContext>,

    segment_id: u64,
    last_start_pts: i64,

    notified_started: bool,
    notified_stopped: bool,
}

impl StreamPuller {
    pub fn new(
        stream_info: StreamInfo,
        stream_msg_tx: mpsc::UnboundedSender<StreamMessage>,
        flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
        stop_signal: Arc<AtomicBool>,
    ) -> Self {
        Self {
            stream_info,
            stream_msg_tx,
            flv_packet_tx,
            stop_signal,
            srt_input: None,
            flv_output: None,
            hls_output: None,
            segment_id: 1,
            last_start_pts: 0,
            notified_started: false,
            notified_stopped: false,
        }
    }

    fn srt_input(&self) -> Result<&SrtInputContext> {
        self.srt_input
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("SRT input context not initialized"))
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

    /// Determines if a new segment should be created based on packet and duration.
    fn should_segment(&self, packet: &Packet) -> Option<i64> {
        let input_ctx = self.srt_input.as_ref()?;

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

    fn notify_stream_started(&self) -> bool {
        let live_id = self.stream_info.live_id();
        if let Err(e) = self
            .stream_msg_tx
            .send(StreamMessage::stream_started(live_id))
        {
            warn!("Failed to send stream connected event: {}", e);
            return false;
        }
        true
    }

    fn notify_stream_stopped(&self, error: Option<String>) {
        let live_id = self.stream_info.live_id();

        let msg =
            StreamMessage::stream_stopped(live_id, error.clone(), self.stream_info.cache_dir());
        if let Err(e) = self.stream_msg_tx.send(msg) {
            warn!("Failed to send stream terminate event: {}", e);
        }

        let flv_output = self
            .flv_output
            .as_ref()
            .map(|o| o.get_flv_packet_sender())
            .flatten();
        if let Some(tx) = flv_output {
            if let Err(e) = tx.send(FlvPacket::EndOfStream {
                live_id: live_id.to_string(),
            }) {
                warn!("Failed to send end of stream packet: {}", e);
            }
        }
    }

    fn notify_segment_complete(&self) {
        let output_ctx = match self.hls_output() {
            Ok(ctx) => ctx,
            Err(_) => return,
        };

        let live_id = self.stream_info.live_id();
        let event = StreamMessage::segment_complete(live_id, output_ctx.path());
        if let Err(e) = self.stream_msg_tx.send(event) {
            warn!("Failed to send final segment complete event: {}", e);
        }
    }

    pub fn start(&mut self) -> Result<()> {
        let result = self.start_impl();

        if !self.notified_stopped {
            self.notify_stream_stopped(result.as_ref().err().map(|e| e.to_string()));
            self.notified_stopped = true;
        }

        result
    }

    /// Main loop for pulling SRT stream, segmenting, and writing to disk.
    fn start_impl(&mut self) -> Result<()> {
        let live_id = self.stream_info.live_id();
        let cache_dir = self.stream_info.cache_dir();

        self.srt_input = Some(SrtInputContext::open(
            &self.stream_info.srt_listener_url(),
            self.stop_signal.clone(),
        )?);

        self.flv_output = Some(FlvOutputContext::create(
            live_id.to_string(),
            self.flv_packet_tx.clone(),
            self.srt_input()?,
        )?);

        self.hls_output = Some(HlsOutputContext::create_segment(
            &cache_dir,
            self.srt_input()?,
            self.segment_id,
        )?);

        while !self.stop_signal.load(Ordering::Relaxed) {
            let packet = Packet::alloc()?;
            if packet.read_safely(self.srt_input()?) == 0 {
                info!("Stream ended for {}", live_id);
                self.notify_stream_stopped(None);
                self.notified_stopped = true;
                break;
            }

            let cloned_packet = packet.clone();

            // Send stream started event on first successful packet read
            if !self.notified_started {
                self.notified_started = self.notify_stream_started();
            }

            if let Some(pts) = self.should_segment(&packet) {
                self.notify_segment_complete();

                self.last_start_pts = pts;
                self.segment_id += 1;

                self.hls_output = Some(HlsOutputContext::create_segment(
                    &cache_dir,
                    self.srt_input()?,
                    self.segment_id,
                )?);
            }

            packet.rescale_ts_for_ctx(self.srt_input()?, self.flv_output()?)?;
            if let Err(e) = packet.write(self.flv_output()?) {
                warn!("Failed to write packet to FLV output: {}", e);
                self.notify_segment_complete();
                anyhow::bail!("Failed to write packet to FLV output: {}", e);
            }

            cloned_packet.rescale_ts_for_ctx(self.srt_input()?, self.hls_output()?)?;
            if let Err(e) = cloned_packet.write(self.hls_output()?) {
                warn!("Failed to write packet to TS output: {}", e);
                self.notify_segment_complete();
                anyhow::bail!("Failed to write packet to TS output: {}", e);
            }
        }

        self.notify_segment_complete();

        Ok(())
    }
}
