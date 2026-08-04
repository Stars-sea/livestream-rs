//! RTSP server — TCP listener for RTSP ingest connections.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use dashmap::DashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::config::ServerConfig;
use crate::flv::hub::FlvEgressHub;
use crate::lifecycle::HandlerLifecycle;
use crate::protocol_server::ProtocolServerCore;
use crate::source::rtsp::{RawRtpFrame, RtspSource};
use rtsp_types::{Message, StatusCode};

use super::rtp::{RtpInterleavedReader, RtpReadItem};
use super::session::{self, RtspSession};

use livestream_codec::{RtpPacket, SegmentConfig};
use livestream_core::config::TranscodeConfig;
use livestream_core::pad::PadSender;
use livestream_core::traits::Source;
use livestream_core::types::Protocol;

use livestream_pipeline::factory;
use livestream_pipeline::sink::minio::ObjectUploader;

pub struct RtspServer {
    core: ProtocolServerCore,
}

impl RtspServer {
    pub async fn create(cfg: ServerConfig) -> Result<Self> {
        let core = ProtocolServerCore::from_config(cfg).await?;
        Ok(Self { core })
    }

    pub async fn run(mut self) -> Result<()> {
        let pending = self.core.pending_lifecycle.clone();
        let hub = self.core.flv_egress_hub.clone();
        let minio = self.core.minio.clone();
        let seg_cfg = self.core.segment_cfg.clone();
        let transcode_cfg = self.core.transcode.clone();

        self.core
            .run(Protocol::Rtsp, move |socket, addr| {
                debug!(client_addr = %addr, "Accepted new RTSP connection");
                Box::pin(spawn_connection_handler(
                    socket,
                    pending.clone(),
                    hub.clone(),
                    minio.clone(),
                    seg_cfg.clone(),
                    transcode_cfg.clone(),
                ))
            })
            .await
    }
}

// ── RTSP message I/O ──

/// Reads into `buf`, failing with the idle-timeout error when no data
/// arrives within IDLE_TIMEOUT.
async fn read_with_idle_timeout(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    buf: &mut [u8],
) -> Result<usize> {
    match tokio::time::timeout(IDLE_TIMEOUT, reader.read(buf)).await {
        Err(_) => anyhow::bail!(
            "RTSP idle timeout: no data received for {}s",
            IDLE_TIMEOUT.as_secs()
        ),
        Ok(res) => Ok(res?),
    }
}

async fn read_message(reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];

    loop {
        let n = read_with_idle_timeout(reader, &mut tmp).await?;
        if n == 0 {
            anyhow::bail!("RTSP connection closed by client before complete request");
        }
        // Cap header growth: a client that never sends \r\n\r\n must not be
        // able to grow `buf` without bound (OOM DoS).
        if buf.len().saturating_add(n) > MAX_RTSP_MESSAGE_SIZE {
            anyhow::bail!(
                "RTSP message header exceeds maximum size {} bytes",
                MAX_RTSP_MESSAGE_SIZE
            );
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

/// Idle timeout for RTSP connections: a client that sends no data for this
/// long is dropped, defending against slowloris-style slot exhaustion.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

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
        let n = read_with_idle_timeout(reader, tmp).await?;
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
    transcode_cfg: TranscodeConfig,
) {
    let cancel_token = CancellationToken::new();
    if let Err(e) = handle_connection(
        stream,
        pending_lifecycle,
        hub,
        minio,
        segment_cfg,
        transcode_cfg,
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
    /// Session state machine, carried into the RTP loop so in-band requests
    /// (TEARDOWN, OPTIONS, …) are answered with the same rules as the handshake.
    session: RtspSession,
    sdp: Option<String>,
    codec_params: Vec<livestream_core::types::CodecParams>,
    live_id: Option<String>,
}

/// A handshake that did not reach recording (client teardown, read/parse
/// failure, or response write failure).
fn handshake_aborted(session: RtspSession) -> HandshakeOutcome {
    HandshakeOutcome {
        recording: false,
        session,
        sdp: None,
        codec_params: vec![],
        live_id: None,
    }
}

/// Handles one handshake request through the session state machine and
/// writes the response. Returns `Err` when the response could not be
/// serialized or written — the handshake must end.
async fn handle_handshake_request(
    session: &mut RtspSession,
    request: &rtsp_types::Request<Vec<u8>>,
    write_half: &mut tokio::net::tcp::OwnedWriteHalf,
) -> Result<()> {
    let cseq = extract_cseq(request);
    match session.handle_request(request) {
        Ok(Some(resp)) => {
            let resp_bytes = session::serialize_response(&resp).map_err(|e| {
                error!(error = %e, "Failed to serialize RTSP response");
                e
            })?;
            write_half.write_all(&resp_bytes).await.map_err(|e| {
                debug!(error = %e, "RTSP handshake: write failed (client likely disconnected)");
                e
            })?;
        }
        Ok(None) => { /* request consumed without response */ }
        Err(e) => {
            error!(error = %e, cseq = cseq, "RTSP session error");
            let err_resp = session::error_response(StatusCode::InternalServerError, cseq);
            if let Ok(resp_bytes) = session::serialize_response(&err_resp) {
                let _ = write_half.write_all(&resp_bytes).await;
            }
        }
    }
    Ok(())
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
                return Ok(handshake_aborted(session));
            }
        };
        let request = match session::parse_message(&data) {
            Ok(Message::Request(req)) => req,
            Ok(_) => continue,
            Err(e) => {
                error!(error = %e, "Failed to parse RTSP request");
                return Ok(handshake_aborted(session));
            }
        };
        if handle_handshake_request(&mut session, &request, write_half)
            .await
            .is_err()
        {
            return Ok(handshake_aborted(session));
        }
        if session.is_teardown() {
            break;
        }
        if session.is_recording() {
            let live_id = session.live_id().map(String::from);
            let sdp = session.sdp_body().map(String::from);
            let codec_params = session.codec_params().unwrap_or(&[]).to_vec();
            info!(live_id = ?live_id, "RTSP session recording, building pipeline");
            return Ok(HandshakeOutcome {
                recording: true,
                session,
                sdp,
                codec_params,
                live_id,
            });
        }
    }

    Ok(handshake_aborted(session))
}

// ── In-band request handling ──

/// Handles an RTSP request received in-band while the session is recording
/// (RFC 2326 §10.12: the client may send requests on the same TCP connection
/// between interleaved frames). Returns `true` when the session must be torn
/// down (TEARDOWN was received and acknowledged).
async fn handle_inband_request(
    data: &[u8],
    session: &mut RtspSession,
    write_half: &mut tokio::net::tcp::OwnedWriteHalf,
) -> bool {
    let request = match session::parse_message(data) {
        Ok(Message::Request(req)) => req,
        Ok(_) => return false,
        Err(e) => {
            warn!(error = %e, "Failed to parse in-band RTSP request");
            return false;
        }
    };
    let cseq = extract_cseq(&request);

    match session.handle_request(&request) {
        Ok(Some(resp)) => {
            if let Ok(resp_bytes) = session::serialize_response(&resp)
                && let Err(e) = write_half.write_all(&resp_bytes).await
            {
                debug!(error = %e, "RTSP in-band response write failed (client likely disconnected)");
            }
        }
        Ok(None) => { /* request consumed without response */ }
        Err(e) => {
            error!(error = %e, cseq = cseq, "RTSP in-band session error");
            let err_resp = session::error_response(StatusCode::InternalServerError, cseq);
            if let Ok(resp_bytes) = session::serialize_response(&err_resp) {
                let _ = write_half.write_all(&resp_bytes).await;
            }
        }
    }

    session.is_teardown()
}

/// Validates one interleaved RTP frame and forwards it to the source
/// channel. Returns `false` when the frame receiver is closed and the RTP
/// read loop must stop; malformed frames are dropped with a warning.
async fn forward_interleaved_frame(
    live_id: &str,
    channel: u8,
    payload: Vec<u8>,
    frame_tx: &tokio::sync::mpsc::Sender<RawRtpFrame>,
) -> bool {
    if payload.len() < 12 {
        warn!(live_id = %live_id, "RTP frame too short: {} bytes", payload.len());
        return true;
    }

    let Some(hdr) = livestream_codec::rtp::parse_rtp_header(&payload) else {
        warn!(live_id = %live_id, "RTP header parse failed");
        return true;
    };
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
        return false;
    }
    true
}

/// Runs the interleaved RTP read loop, dispatching frames and in-band RTSP
/// requests until the stream ends (cancel, idle timeout, TEARDOWN, EOF, or a
/// closed frame receiver).
async fn run_rtp_loop(
    live_id: &str,
    rtp_reader: &mut RtpInterleavedReader<tokio::net::tcp::OwnedReadHalf>,
    session: &mut RtspSession,
    write_half: &mut tokio::net::tcp::OwnedWriteHalf,
    frame_tx: &tokio::sync::mpsc::Sender<RawRtpFrame>,
    cancel: &CancellationToken,
) {
    info!(live_id = %live_id, "Reading RTP interleaved frames (in-band RTSP requests handled)");
    let read_cancel = cancel.clone();
    let idle = tokio::time::sleep(IDLE_TIMEOUT);
    tokio::pin!(idle);
    'rtp_loop: loop {
        tokio::select! {
            biased;
            _ = read_cancel.cancelled() => break 'rtp_loop,
            result = rtp_reader.next_item() => {
                idle.as_mut().reset(tokio::time::Instant::now() + IDLE_TIMEOUT);
                if !handle_rtp_item(result, live_id, session, write_half, frame_tx).await {
                    break 'rtp_loop;
                }
            }
            _ = &mut idle => {
                warn!(
                    live_id = %live_id,
                    "RTSP idle timeout: no data received for {}s, disconnecting",
                    IDLE_TIMEOUT.as_secs()
                );
                break 'rtp_loop;
            }
        }
    }
}

/// Handles one item from the RTP read loop. Returns `false` when the loop
/// must stop (TEARDOWN, read error/EOF, or a closed frame receiver).
async fn handle_rtp_item(
    item: Result<RtpReadItem>,
    live_id: &str,
    session: &mut RtspSession,
    write_half: &mut tokio::net::tcp::OwnedWriteHalf,
    frame_tx: &tokio::sync::mpsc::Sender<RawRtpFrame>,
) -> bool {
    match item {
        Ok(RtpReadItem::Interleaved { channel, payload }) => {
            forward_interleaved_frame(live_id, channel, payload, frame_tx).await
        }
        Ok(RtpReadItem::RtspRequest(data)) => {
            if handle_inband_request(&data, session, write_half).await {
                info!(live_id = %live_id, "RTSP TEARDOWN received in-band, stopping stream");
                false
            } else {
                true
            }
        }
        Ok(RtpReadItem::Stray(bytes)) => {
            warn!(
                live_id = %live_id,
                "Ignoring {} stray non-interleaved bytes",
                bytes.len()
            );
            true
        }
        Err(e) => {
            debug!(error = %e, "RTP read loop ended (stream teardown)");
            false
        }
    }
}

// ── Main connection handler ──

#[allow(clippy::too_many_arguments)]
async fn handle_connection(
    stream: TcpStream,
    pending_lifecycle: Arc<DashMap<String, HandlerLifecycle>>,
    hub: Arc<FlvEgressHub>,
    minio: Arc<dyn ObjectUploader>,
    segment_cfg: SegmentConfig,
    transcode_cfg: TranscodeConfig,
    cancel_token: CancellationToken,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let HandshakeOutcome {
        recording,
        mut session,
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
    if let Err(e) = lifecycle.connect().await {
        // Cleanup must run on every exit path so the FLV channel and cancel
        // token do not leak when the connect fails.
        cancel.cancel();
        hub.remove_channel(&live_id);
        return Err(e);
    }
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
        &transcode_cfg,
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

    let mut rtp_reader = RtpInterleavedReader::new(reader.into_inner());
    run_rtp_loop(
        &live_id,
        &mut rtp_reader,
        &mut session,
        &mut write_half,
        &frame_tx,
        &cancel,
    )
    .await;

    lifecycle.disconnect();
    cancel.cancel();
    hub.remove_channel(&live_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtsp_types::{Method, Request, Version};

    /// Drives a session through ANNOUNCE → SETUP → RECORD.
    fn recording_session() -> RtspSession {
        let mut session = RtspSession::new();
        let announce = Request::builder(Method::Announce, Version::V1_0)
            .typed_header(&rtsp_types::headers::CSeq::from(1u32))
            .build(b"v=0\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\n".to_vec());
        session.handle_request(&announce).unwrap();
        let setup = Request::builder(Method::Setup, Version::V1_0)
            .request_uri(rtsp_types::Url::parse("rtsp://example.com/live/track1").unwrap())
            .typed_header(&rtsp_types::headers::CSeq::from(2u32))
            .build(b"".to_vec());
        session.handle_request(&setup).unwrap();
        let record = Request::builder(Method::Record, Version::V1_0)
            .typed_header(&rtsp_types::headers::CSeq::from(3u32))
            .build(b"".to_vec());
        session.handle_request(&record).unwrap();
        session
    }

    /// A (server write half, client read half) loopback pair.
    async fn loopback_pair() -> (
        tokio::net::tcp::OwnedWriteHalf,
        tokio::net::tcp::OwnedReadHalf,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (server_stream, _) = listener.accept().await.unwrap();
        let (_, server_write) = server_stream.into_split();
        let (client_read, _) = client.into_split();
        (server_write, client_read)
    }

    /// Reads the server's response on the loopback pair until the header
    /// terminator (a single read may not carry the whole response).
    async fn read_response(client_read: &mut tokio::net::tcp::OwnedReadHalf) -> Vec<u8> {
        let mut resp = Vec::new();
        while !resp.ends_with(b"\r\n\r\n") {
            client_read.read_buf(&mut resp).await.unwrap();
        }
        resp
    }

    #[tokio::test]
    async fn inband_teardown_after_record_gets_200_and_tears_down() {
        let (mut server_write, mut client_read) = loopback_pair().await;
        let mut session = recording_session();
        assert!(session.is_recording());

        // A TEARDOWN sent on the same connection after RECORD must be
        // answered, not consumed byte-by-byte as interleaved frames.
        let teardown: &[u8] =
            b"TEARDOWN rtsp://example.com/live/track1 RTSP/1.0\r\nCSeq: 4\r\n\r\n";
        let torn_down = handle_inband_request(teardown, &mut session, &mut server_write).await;
        assert!(torn_down, "TEARDOWN must tear the session down");

        let resp = read_response(&mut client_read).await;
        let resp = String::from_utf8_lossy(&resp);
        assert!(resp.starts_with("RTSP/1.0 200 Ok"), "got: {resp}");
        assert!(resp.contains("CSeq: 4"), "got: {resp}");
    }

    #[tokio::test]
    async fn inband_options_keeps_session_alive() {
        let (mut server_write, mut client_read) = loopback_pair().await;
        let mut session = recording_session();

        let opts: &[u8] = b"OPTIONS rtsp://example.com/live/track1 RTSP/1.0\r\nCSeq: 4\r\n\r\n";
        let torn_down = handle_inband_request(opts, &mut session, &mut server_write).await;
        assert!(!torn_down, "OPTIONS must not tear the session down");
        assert!(session.is_recording());

        let resp = read_response(&mut client_read).await;
        let resp = String::from_utf8_lossy(&resp);
        assert!(resp.starts_with("RTSP/1.0 200 Ok"), "got: {resp}");
        assert!(resp.contains("CSeq: 4"), "got: {resp}");
    }

    #[tokio::test]
    async fn inband_unsupported_method_gets_405_without_teardown() {
        let (mut server_write, mut client_read) = loopback_pair().await;
        let mut session = recording_session();

        let pause: &[u8] = b"PAUSE rtsp://example.com/live/track1 RTSP/1.0\r\nCSeq: 4\r\n\r\n";
        let torn_down = handle_inband_request(pause, &mut session, &mut server_write).await;
        assert!(!torn_down, "PAUSE must not tear the session down");

        let resp = read_response(&mut client_read).await;
        let resp = String::from_utf8_lossy(&resp);
        assert!(
            resp.starts_with("RTSP/1.0 405 Method Not Allowed"),
            "got: {resp}"
        );
    }
}
