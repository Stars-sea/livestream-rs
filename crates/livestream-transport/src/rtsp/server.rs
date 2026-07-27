//! RTSP server — TCP listener for RTSP ingest connections.

use std::io::ErrorKind;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::config::ServerConfig;
use anyhow::Result;
use dashmap::DashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::controller::ControlMessage;
use crate::dispatcher::EndReason;
use crate::dispatcher::EventDispatcher;
use crate::flv::hub::FlvEgressHub;
use crate::lifecycle::HandlerLifecycle;
use crate::registry::SessionRegistry;
use crate::registry::state::SessionEndpoint;
use crate::source::rtsp::{RawRtpFrame, RtspSource};
use rtsp_types::Message;

use super::rtp::RtpInterleavedReader;
use super::session::{self, RtspSession};

use livestream_codec::{RtpPacket, SegmentConfig};
use livestream_core::channel::MpscRx;
use livestream_core::pad::PadSender;
use livestream_core::traits::Source;
use livestream_core::types::Protocol;
use livestream_pipeline::factory;
use livestream_pipeline::sink::minio::ObjectUploader;

pub struct RtspServer {
    registry: Arc<SessionRegistry>,
    dispatcher: Arc<EventDispatcher>,
    listener: TcpListener,
    ctrl_channel: MpscRx<ControlMessage>,
    flv_egress_hub: Arc<FlvEgressHub>,
    pending_lifecycle: Arc<DashMap<String, HandlerLifecycle>>,
    precreate_ttl: Duration,
    minio: Arc<dyn ObjectUploader>,
    segment_cfg: SegmentConfig,
    cancel_token: CancellationToken,
}

impl RtspServer {
    pub async fn create(cfg: ServerConfig) -> Result<Self> {
        let listener = TcpListener::bind(cfg.addr).await?;
        Ok(Self {
            listener,
            ctrl_channel: cfg.ctrl_channel,
            registry: cfg.registry,
            dispatcher: cfg.dispatcher,
            flv_egress_hub: cfg.flv_egress_hub,
            pending_lifecycle: Arc::new(DashMap::new()),
            precreate_ttl: cfg.precreate_ttl,
            minio: cfg.minio,
            segment_cfg: cfg.segment_cfg,
            cancel_token: cfg.cancel_token,
        })
    }

    pub async fn run(mut self) -> Result<()> {
        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    debug!("RTSP server cancellation requested, shutting down");
                    break;
                }

                msg = self.ctrl_channel.recv() => {
                    match msg {
                        Some(msg) => {
                            if let Err(e) = self.handle_control_message(msg).await {
                                error!(error = %e, "Failed to handle RTSP control message");
                            }
                        }
                        None => {
                            // Channel closed — sleep to avoid busy-looping.
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }

                accept_res = self.listener.accept() => {
                    self.handle_accept_result(accept_res).await;
                }
            }
        }

        Ok(())
    }
    fn accept_client(&self, socket: TcpStream, addr: SocketAddr) {
        debug!(client_addr = %addr, "Accepted new RTSP connection");

        tokio::spawn(spawn_connection_handler(
            socket,
            self.pending_lifecycle.clone(),
            self.flv_egress_hub.clone(),
            self.minio.clone(),
            self.segment_cfg.clone(),
        ));
    }
    async fn handle_accept_result(&mut self, accept_res: std::io::Result<(TcpStream, SocketAddr)>) {
        fn is_retryable_accept_error(err: &std::io::Error) -> bool {
            matches!(
                err.kind(),
                ErrorKind::Interrupted
                    | ErrorKind::WouldBlock
                    | ErrorKind::TimedOut
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::ConnectionReset
            )
        }

        match accept_res {
            Ok((socket, addr)) => self.accept_client(socket, addr),
            Err(err) if is_retryable_accept_error(&err) => {
                warn!(error = %err, kind = ?err.kind(), "Retryable RTSP accept error, server continues running");
                sleep(Duration::from_millis(20)).await;
            }
            Err(err) => {
                error!(error = %err, kind = ?err.kind(), "Non-retryable RTSP accept error, server stays alive with backoff");
                sleep(Duration::from_millis(200)).await;
            }
        }
    }

    async fn handle_control_message(&mut self, msg: ControlMessage) -> Result<()> {
        match msg {
            ControlMessage::PrecreateStream { live_id, .. } => {
                // Pre-create the FLV broadcast channel so subscribers can join
                // before the publisher connects.
                self.flv_egress_hub.create_channel(&live_id);

                let session_token = self.cancel_token.child_token();

                let lifecycle = HandlerLifecycle::new(
                    live_id.clone(),
                    Protocol::Rtsp,
                    self.registry.clone(),
                    self.dispatcher.clone(),
                );
                lifecycle
                    .pending(SessionEndpoint::default(), session_token.clone())
                    .await?;

                self.spawn_precreate_session_ttl(live_id, lifecycle, session_token);

                Ok(())
            }
            ControlMessage::StopStream { live_id } => {
                if let Some(token) = self.registry.get_cancel_token(&live_id) {
                    token.cancel();
                }

                Ok(())
            }
        }
    }

    fn spawn_precreate_session_ttl(
        &mut self,
        live_id: String,
        lifecycle: HandlerLifecycle,
        session_token: CancellationToken,
    ) {
        let pending_lifecycle = self.pending_lifecycle.clone();
        pending_lifecycle.insert(live_id.clone(), lifecycle);

        let ttl = self.precreate_ttl;
        if ttl.is_zero() {
            debug!(
                "Precreate session TTL is set to 0, skipping TTL expiration for live_id {}",
                live_id
            );
            return;
        }

        tokio::spawn(async move {
            tokio::select! {
                _ = session_token.cancelled() => { return; }
                _ = sleep(ttl) => {}
            }

            if !pending_lifecycle.contains_key(&live_id) {
                return;
            }

            warn!(
                live_id = %live_id,
                ttl_secs = ttl.as_secs(),
                "Expired pending RTSP precreated session by TTL"
            );

            let Some((_, lifecycle)) = pending_lifecycle.remove(&live_id) else {
                debug!(live_id = %live_id, "Pending lifecycle already removed for live_id, skipping TTL expiration");
                return;
            };
            lifecycle.disconnect_with_reason(EndReason::Timeout);
        });
    }
}

// ── RTSP message I/O ──

async fn read_message(reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];

    loop {
        let n = reader.read(&mut tmp).await?;
        if n == 0 {
            anyhow::bail!("RTSP connection closed by client before complete request");
        }
        buf.extend_from_slice(&tmp[..n]);

        if let Some(pos) = find_header_end(&buf) {
            let content_length = extract_content_length(&buf[..pos])?;
            let total = pos
                .checked_add(4)
                .and_then(|v| v.checked_add(content_length))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "RTSP message size overflow: pos={pos}, content_length={content_length}"
                    )
                })?;
            if buf.len() < total {
                read_body_until(&mut buf, reader, &mut tmp, total).await?;
            }
            buf.truncate(total);
            return Ok(buf);
        }
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Maximum RTSP message size (headers + body). RTSP messages are small;
/// SDP bodies rarely exceed 16 KiB. A 64 KiB cap defends against OOM.
const MAX_RTSP_MESSAGE_SIZE: usize = 64 * 1024;

fn extract_content_length(buf: &[u8]) -> Result<usize> {
    let headers = String::from_utf8_lossy(buf);
    let raw: usize = headers
        .lines()
        .find_map(|line| {
            if line.trim().to_lowercase().starts_with("content-length:") {
                line.split(':').nth(1).and_then(|v| v.trim().parse().ok())
            } else {
                None
            }
        })
        .unwrap_or(0);
    if raw > MAX_RTSP_MESSAGE_SIZE {
        anyhow::bail!(
            "RTSP Content-Length {} exceeds maximum {}",
            raw,
            MAX_RTSP_MESSAGE_SIZE
        );
    }
    Ok(raw)
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
            anyhow::bail!("RTSP connection closed before complete body");
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Ok(())
}

// ── Connection handler ──

async fn spawn_connection_handler(
    stream: TcpStream,
    pending_lifecycle: Arc<DashMap<String, HandlerLifecycle>>,
    hub: Arc<FlvEgressHub>,
    minio: Arc<dyn ObjectUploader>,
    segment_cfg: SegmentConfig,
) {
    let cancel_token = CancellationToken::new();
    if let Err(e) = handle_connection(
        stream,
        pending_lifecycle,
        hub,
        minio,
        segment_cfg,
        cancel_token,
    )
    .await
    {
        error!(error = %e, "RTSP connection error");
    }
}

// ── RTSP handshake ──

fn extract_cseq(request: &rtsp_types::Request<Vec<u8>>) -> u32 {
    request
        .typed_header::<rtsp_types::headers::CSeq>()
        .ok()
        .flatten()
        .map(|c| *c)
        .unwrap_or(0)
}

struct HandshakeOutcome {
    recording: bool,
    sdp: Option<String>,
    codec_params: Vec<livestream_core::types::CodecParams>,
    live_id: Option<String>,
}

/// Runs the RTSP handshake (OPTIONS/ANNOUNCE/SETUP/RECORD).
/// Returns the outcome when the handshake completes (teardown or recording).
async fn run_rtsp_handshake(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    write_half: &mut tokio::net::tcp::OwnedWriteHalf,
) -> Result<HandshakeOutcome> {
    let mut session = RtspSession::new();

    loop {
        let data = match read_message(reader).await {
            Ok(d) => d,
            Err(e) => {
                warn!(error = %e, "RTSP read error during handshake");
                return Ok(HandshakeOutcome {
                    recording: false,
                    sdp: None,
                    codec_params: vec![],
                    live_id: None,
                });
            }
        };

        let request = match session::parse_message(&data) {
            Ok(Message::Request(req)) => req,
            Ok(_) => continue,
            Err(e) => {
                error!(error = %e, "Failed to parse RTSP request");
                return Ok(HandshakeOutcome {
                    recording: false,
                    sdp: None,
                    codec_params: vec![],
                    live_id: None,
                });
            }
        };

        let cseq = extract_cseq(&request);

        match session.handle_request(&request) {
            Ok(Some(resp)) => {
                let resp_bytes = match session::serialize_response(&resp) {
                    Ok(b) => b,
                    Err(e) => {
                        error!(error = %e, "Failed to serialize RTSP response");
                        return Ok(HandshakeOutcome {
                            recording: false,
                            sdp: None,
                            codec_params: vec![],
                            live_id: None,
                        });
                    }
                };
                if let Err(e) = write_half.write_all(&resp_bytes).await {
                    debug!(error = %e, "RTSP handshake: write failed (client likely disconnected)");
                    return Ok(HandshakeOutcome {
                        recording: false,
                        sdp: None,
                        codec_params: vec![],
                        live_id: None,
                    });
                }
            }
            Ok(None) => { /* request consumed without response */ }
            Err(e) => {
                error!(error = %e, cseq = cseq, "RTSP session error");
                let err_resp =
                    session::error_response(rtsp_types::StatusCode::InternalServerError, cseq);
                if let Ok(resp_bytes) = session::serialize_response(&err_resp) {
                    let _ = write_half.write_all(&resp_bytes).await;
                }
            }
        }

        if session.is_teardown() {
            break;
        }

        if session.is_recording() {
            let live_id = session.live_id().map(String::from);
            info!(live_id = ?live_id, "RTSP session recording, building pipeline");
            let outcome = HandshakeOutcome {
                recording: true,
                sdp: session.sdp_body().map(String::from),
                codec_params: session.codec_params().unwrap_or(&[]).to_vec(),
                live_id,
            };
            return Ok(outcome);
        }
    }

    Ok(HandshakeOutcome {
        recording: false,
        sdp: None,
        codec_params: vec![],
        live_id: None,
    })
}

// ── Main connection handler ──

async fn handle_connection(
    stream: TcpStream,
    pending_lifecycle: Arc<DashMap<String, HandlerLifecycle>>,
    hub: Arc<FlvEgressHub>,
    minio: Arc<dyn ObjectUploader>,
    segment_cfg: SegmentConfig,
    cancel_token: CancellationToken,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let HandshakeOutcome {
        recording,
        sdp,
        codec_params,
        live_id,
    } = run_rtsp_handshake(&mut reader, &mut write_half).await?;

    if !recording {
        return Ok(());
    }

    let live_id = live_id.ok_or_else(|| {
        anyhow::anyhow!("RTSP ANNOUNCE did not provide a recognizable stream path")
    })?;
    let sdp = sdp.ok_or_else(|| anyhow::anyhow!("SDP missing"))?;

    let Some((_, lifecycle)) = pending_lifecycle.remove(&live_id) else {
        anyhow::bail!("No precreated session found for live_id: {}", live_id);
    };

    let cancel = cancel_token.child_token();
    hub.create_channel(&live_id);

    let (rtp_tx, rtp_rx) = PadSender::<RtpPacket>::new_channel(1024);
    let (source, frame_tx) =
        RtspSource::new(&live_id, codec_params.clone(), rtp_tx, cancel.clone());

    // Transition lifecycle: PENDING → CONNECTING → CONNECTED.
    lifecycle.connect().await?;
    let source = Arc::new(source);

    // Build pipeline BEFORE spawning source so consumers are ready.
    // Keep alive during RTP read loop — dropping PipelineImpl detaches
    // JoinHandle tasks, which is fine, but we hold it for explicitness.
    let _pipeline = factory::build_rtsp_pipeline(
        &live_id,
        rtp_rx,
        &sdp,
        &codec_params,
        hub.clone(),
        minio,
        &segment_cfg,
        cancel.clone(),
    );

    if let Err(ref e) = _pipeline {
        warn!(live_id = %live_id, error = %e, "RTSP pipeline factory failed, stream will have no HLS output");
    }

    // Spawn RTSP source AFTER pipeline is built.
    tokio::spawn({
        let source = source.clone();
        async move {
            if let Err(e) = source.start().await {
                error!(error = %e, "RtspSource failed");
            }
        }
    });

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
                        debug!(error = %e, "RTP read loop ended (stream teardown)");
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
                    None => warn!(live_id = %live_id, "RTP header parse failed"),
                }
            }
        }
    }

    lifecycle.disconnect();
    cancel.cancel();
    hub.remove_channel(&live_id);
    Ok(())
}
