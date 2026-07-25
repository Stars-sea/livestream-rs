//! RTSP server — TCP listener for RTSP ingest connections.
//!
//! After the RECORD handshake, builds the pipeline chain:
//!   RtpInterleavedReader → RtspSource → RtpDepackProcessor → FlvMux → FlvSink
//!
//! Processor consume loops are spawned as tokio tasks (temporary — will be
//! replaced by PipelineImpl engine task spawning in a future phase).

use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::flv::hub::FlvEgressHub;
use crate::source::rtsp::{RawRtpFrame, RtspSource};

use super::rtp::RtpInterleavedReader;
use super::session::{self, RtspSession};

use livestream_codec::{EncodedPacket, RtpPacket};
use livestream_core::{
    pad::PadSender,
    traits::{Processor, Sink, Source},
};
use livestream_media::flv::FlvTag;
use livestream_pipeline::broadcast::FlvBroadcast;
use livestream_pipeline::processor::{FlvMux, RtpDemuxProcessor};
use livestream_pipeline::sink::FlvSink;

/// RTSP server listening on a TCP port.
pub struct RtspServer {
    listener: TcpListener,
}

impl RtspServer {
    pub async fn bind(addr: &str) -> Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        info!("RTSP server listening on {}", addr);
        Ok(Self { listener })
    }

    /// Accept connections and handle each one.
    ///
    /// `hub` provides channel management (create/remove) and FLV broadcast.
    pub async fn serve(&self, hub: Arc<FlvEgressHub>) -> Result<()> {
        loop {
            let (stream, addr) = self.listener.accept().await?;
            info!("RTSP connection from {}", addr);
            let hub = Arc::clone(&hub);
            let live_id = format!("rtsp-{}", addr);
            tokio::spawn(spawn_connection(stream, live_id, hub));
        }
    }
}

/// Read until we have a complete RTSP message (headers end with \r\n\r\n).
async fn read_message(reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];

    loop {
        let n = reader.read(&mut tmp).await?;
        if n == 0 {
            anyhow::bail!("Connection closed");
        }
        buf.extend_from_slice(&tmp[..n]);

        if let Some(pos) = find_header_end(&buf) {
            let content_len = extract_content_length(&buf[..pos + 4]);
            let total = if content_len > 0 {
                pos + 4 + content_len
            } else {
                pos + 4
            };
            read_body_until(&mut buf, reader, &mut tmp, total).await?;
            return Ok(buf[..total].to_vec());
        }
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn extract_content_length(buf: &[u8]) -> usize {
    let text = String::from_utf8_lossy(buf);
    for line in text.lines() {
        let trimmed = line.trim().to_lowercase();
        if let Some(val) = trimmed.strip_prefix("content-length:") {
            return val.trim().parse().unwrap_or(0);
        }
    }
    0
}

async fn read_body_until(
    buf: &mut Vec<u8>,
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    tmp: &mut [u8; 4096],
    total: usize,
) -> Result<()> {
    while buf.len() < total {
        let n = reader.read(tmp).await?;
        if n == 0 {
            anyhow::bail!("Connection closed");
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Ok(())
}

async fn spawn_connection(stream: TcpStream, live_id: String, hub: Arc<FlvEgressHub>) {
    if let Err(e) = handle_connection(stream, &live_id, hub).await {
        error!(error = %e, live_id = %live_id, "RTSP connection error");
    }
}

async fn write_error_response(
    write_half: &mut tokio::net::tcp::OwnedWriteHalf,
    cseq: u32,
) {
    let err_resp = session::error_response(rtsp_types::StatusCode::InternalServerError, cseq);
    if let Ok(resp_bytes) = session::serialize_response(&err_resp) {
        let _ = write_half.write_all(&resp_bytes).await;
    }
}

fn spawn_source_start(source: Arc<RtspSource>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = source.start().await {
            error!(error = %e, "RtspSource failed");
        }
    })
}

async fn handle_connection(stream: TcpStream, live_id: &str, hub: Arc<FlvEgressHub>) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut session = RtspSession::new();

    // ── Phase 1: RTSP handshake (text mode) ──

    loop {
        let raw = match read_message(&mut reader).await {
            Ok(data) => data,
            Err(e) => {
                warn!(error = %e, "Failed to read RTSP message");
                break;
            }
        };

        let message = match session::parse_message(&raw) {
            Ok(msg) => msg,
            Err(e) => {
                warn!(error = %e, "Failed to parse RTSP message");
                let err_resp = session::error_response(rtsp_types::StatusCode::BadRequest, 0);
                let resp_bytes = session::serialize_response(&err_resp)?;
                write_half.write_all(&resp_bytes).await?;
                continue;
            }
        };

        match message {
            rtsp_types::Message::Request(request) => {
                let method = request.method().clone();
                match session.handle_request(&request) {
                    Ok(Some(response)) => {
                        let resp_bytes = session::serialize_response(&response)?;
                        write_half.write_all(&resp_bytes).await?;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        let cseq = request
                            .typed_header::<rtsp_types::headers::CSeq>()
                            .ok()
                            .flatten()
                            .map(|c| *c)
                            .unwrap_or(0);
                        error!(error = %e, method = ?method, "RTSP handler error");
                        write_error_response(&mut write_half, cseq).await;
                    }
                }

                if session.is_teardown() {
                    break;
                }

                if session.is_recording() {
                    info!(live_id = %live_id, "RTSP session recording, building pipeline");
                    break;
                }
            }
            rtsp_types::Message::Data(data) => {
                warn!(
                    channel = data.channel_id(),
                    "Unexpected data message during RTSP handshake"
                );
            }
            _ => {}
        }
    }

    // ── Phase 2: Build pipeline + enter RTP read loop ──

    if session.is_recording() {
        let sdp = session
            .sdp_body()
            .ok_or_else(|| anyhow::anyhow!("No SDP available — ANNOUNCE not received"))?;
        let codec_params = session.codec_params().unwrap_or(&[]).to_vec();

        // Create cancel token for coordinated shutdown.
        let cancel = CancellationToken::new();

        // ── Create FLV channel in the egress hub ──
        hub.create_channel(live_id);

        // Broadcast trait object for FlvSink.
        let broadcast: Arc<dyn FlvBroadcast> = hub.clone();

        // ── Create pad pairs ──

        // RtspSource → RtpDepackProcessor
        let (rtp_tx, rtp_rx) = PadSender::<RtpPacket>::new_channel(256);

        // RtpDepackProcessor → FlvMux
        let (enc_tx, enc_rx) = PadSender::<EncodedPacket>::new_channel(256);

        // FlvMux → FlvSink
        let (flv_tx, flv_rx) = PadSender::<FlvTag>::new_channel(256);

        // ── Build pipeline nodes ──

        let depack = Arc::new(RtpDemuxProcessor::new(
            live_id,
            sdp,
            codec_params.clone(),
            rtp_rx,
            vec![enc_tx],
        )?);

        let demand_handle = flv_tx.demand().new_handle();
        let flv_mux = Arc::new(FlvMux::new(live_id, enc_rx, vec![flv_tx]));
        let flv_sink = Arc::new(FlvSink::new(live_id, broadcast, flv_rx, demand_handle));

        // Create RtspSource — frame_tx is held by this handler for feeding
        // RTP frames from the interleaved reader.
        let (source, frame_tx) = RtspSource::new(live_id, codec_params, rtp_tx, cancel.clone());
        let source = Arc::new(source);

        // ── Spawn processor consume loops (temporary pattern) ──

        let depack_task = {
            let depack = Arc::clone(&depack);
            let cancel = cancel.clone();
            tokio::spawn(async move {
                run_processor(depack, cancel).await;
            })
        };

        let flv_mux_task = {
            let flv_mux = Arc::clone(&flv_mux);
            let cancel = cancel.clone();
            tokio::spawn(async move {
                run_processor(flv_mux, cancel).await;
            })
        };

        let flv_sink_task = {
            let flv_sink = Arc::clone(&flv_sink);
            let cancel = cancel.clone();
            tokio::spawn(async move {
                run_sink(flv_sink, cancel).await;
            })
        };

        // ── Spawn source start task ──

        let source_task = spawn_source_start(Arc::clone(&source));

        // ── Phase 3: RTP read loop ──

        let inner = reader.into_inner();
        let mut rtp_reader = RtpInterleavedReader::new(inner);

        info!(live_id = %live_id, "Reading RTP interleaved frames");
        let read_cancel = cancel.clone();
        'rtp_loop: loop {
            tokio::select! {
                _ = read_cancel.cancelled() => break 'rtp_loop,
                result = rtp_reader.next_frame() => {
                    let (channel, payload) = match result {
                        Ok(r) => r,
                        Err(e) => {
                            error!(error = %e, "RTP read error");
                            break 'rtp_loop;
                        }
                    };

                    if payload.len() < 12 {
                        warn!(live_id = %live_id, "RTP frame too short: {} bytes", payload.len());
                        continue;
                    }

                    match livestream_codec::rtp::parse_rtp_header(&payload) {
                        Some(hdr) => {
                            let frame = RawRtpFrame {
                                channel,
                                payload_type: hdr.payload_type,
                                rtp_timestamp: hdr.rtp_timestamp,
                                marker: hdr.marker,
                                sequence_number: hdr.sequence_number,
                                ssrc: hdr.ssrc,
                                rtp_data: payload,
                            };

                            if frame_tx.send(frame).await.is_err() {
                                warn!(live_id = %live_id, "Frame receiver closed");
                                break 'rtp_loop;
                            }
                        }
                        None => {
                            warn!(live_id = %live_id, "Failed to parse RTP header: too short");
                        }
                    }
                }
            }
        }

        // ── Cleanup ──

        info!(live_id = %live_id, "Shutting down RTSP pipeline");
        cancel.cancel();

        // Wait for tasks to finish (with timeout).
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let _ = source_task.await;
            let _ = depack_task.await;
            let _ = flv_mux_task.await;
            let _ = flv_sink_task.await;
        })
        .await;

        // Remove channel from egress hub.
        hub.remove_channel(live_id);
    }

    Ok(())
}

/// Run a processor consume loop. Reads from the processor's input pad,
/// calls `process()`, and forwards results to output pads.
async fn run_processor<P>(processor: Arc<P>, cancel: CancellationToken)
where
    P: Processor + 'static,
    P::Output: Clone,
{
    tracing::debug!(processor = %processor.name(), "Processor loop started");
    loop {
        tokio::select! {
            pkt = processor.input().recv() => {
                let pkt = match pkt {
                    Some(p) => p,
                    None => break,
                };

                if !processor.should_process() {
                    continue;
                }

                match processor.process(pkt).await {
                    Ok(results) => {
                        for item in results {
                            for out_pad in processor.outputs() {
                                if out_pad.send(item.clone()).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            processor = %processor.name(),
                            error = %e,
                            "Processor error, packet dropped"
                        );
                    }
                }
            }
            _ = cancel.cancelled() => break,
        }
    }

    if let Err(e) = processor.close().await {
        tracing::warn!(
            processor = %processor.name(),
            error = %e,
            "Processor close error"
        );
    }
    tracing::debug!(processor = %processor.name(), "Processor loop ended");
}

/// Run a sink consume loop. Reads from the sink's input pad and calls `consume()`.
async fn run_sink<Si>(sink: Arc<Si>, cancel: CancellationToken)
where
    Si: Sink + 'static,
{
    tracing::debug!(sink = %sink.name(), "Sink loop started");
    loop {
        tokio::select! {
            item = sink.input().recv() => {
                let item = match item {
                    Some(i) => i,
                    None => break,
                };

                if let Err(e) = sink.consume(item).await {
                    tracing::warn!(
                        sink = %sink.name(),
                        error = %e,
                        "Sink error, item dropped"
                    );
                }
            }
            _ = cancel.cancelled() => break,
        }
    }
    tracing::debug!(sink = %sink.name(), "Sink loop ended");
}
