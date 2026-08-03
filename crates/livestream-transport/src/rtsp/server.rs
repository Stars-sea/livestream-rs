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
use crate::registry::SessionRegistry;
use crate::source::rtsp::{RawRtpFrame, RtspSource};
use rtsp_types::{Message, Method};

use super::rtp::RtpInterleavedReader;
use super::session::{self, RtspSession};

use livestream_codec::{RtpPacket, SegmentConfig};
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
        let registry = self.core.registry.clone();

        self.core
            .run(Protocol::Rtsp, move |socket, addr| {
                debug!(client_addr = %addr, "Accepted new RTSP connection");
                Box::pin(spawn_connection_handler(
                    socket,
                    pending.clone(),
                    hub.clone(),
                    minio.clone(),
                    seg_cfg.clone(),
                    registry.clone(),
                ))
            })
            .await
    }
}

// ── RTSP message I/O ──

async fn read_message(reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];

    loop {
        let n = match tokio::time::timeout(IDLE_TIMEOUT, reader.read(&mut tmp)).await {
            Err(_) => {
                anyhow::bail!(
                    "RTSP idle timeout: no data received for {}s",
                    IDLE_TIMEOUT.as_secs()
                )
            }
            Ok(res) => res?,
        };
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
        let n = match tokio::time::timeout(IDLE_TIMEOUT, reader.read(tmp)).await {
            Err(_) => {
                anyhow::bail!(
                    "RTSP idle timeout: no data received for {}s",
                    IDLE_TIMEOUT.as_secs()
                )
            }
            Ok(res) => res?,
        };
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
    registry: Arc<SessionRegistry>,
) {
    let cancel_token = CancellationToken::new();
    if let Err(e) = handle_connection(
        stream,
        pending_lifecycle,
        hub,
        minio,
        segment_cfg,
        registry,
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

/// Whether the passphrase presented in the ANNOUNCE URI userinfo matches
/// the precreated session's passphrase. Streams without a passphrase and
/// unknown streams always pass (enforcement is opt-in per stream).
async fn passphrase_matches(registry: &SessionRegistry, session: &RtspSession) -> bool {
    let Some(live_id) = session.live_id() else {
        return true;
    };
    let Some(descriptor) = registry.get_session(live_id) else {
        return true;
    };
    let expected = descriptor.read().await.endpoint.passphrase.clone();
    match expected {
        Some(expected) => session.provided_passphrase() == Some(expected.as_str()),
        None => true,
    }
}

/// Respond 401 to an ANNOUNCE with a wrong passphrase and end the handshake.
async fn deny_announce(
    write_half: &mut tokio::net::tcp::OwnedWriteHalf,
    cseq: u32,
    live_id: Option<&str>,
) -> Result<HandshakeOutcome> {
    let denied = session::error_response(rtsp_types::StatusCode::Unauthorized, cseq);
    if let Ok(resp_bytes) = session::serialize_response(&denied) {
        let _ = write_half.write_all(&resp_bytes).await;
    }
    warn!(
        live_id = ?live_id,
        "RTSP publish rejected: invalid or missing passphrase"
    );
    Ok(HandshakeOutcome {
        recording: false,
        sdp: None,
        codec_params: vec![],
        live_id: None,
    })
}

/// Runs the RTSP handshake (OPTIONS/ANNOUNCE/SETUP/RECORD).
/// Returns the outcome when the handshake completes (teardown or recording).
async fn run_rtsp_handshake(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    write_half: &mut tokio::net::tcp::OwnedWriteHalf,
    registry: &SessionRegistry,
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
                // Passphrase enforcement happens at ANNOUNCE time so the
                // client gets a 401 instead of a success followed by a
                // dropped connection.
                if request.method() == Method::Announce
                    && !passphrase_matches(registry, &session).await
                {
                    return deny_announce(write_half, cseq, session.live_id()).await;
                }
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
    registry: Arc<SessionRegistry>,
    cancel_token: CancellationToken,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let HandshakeOutcome {
        recording,
        sdp,
        codec_params,
        live_id,
        ..
    } = run_rtsp_handshake(&mut reader, &mut write_half, &registry).await?;

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
    let idle = tokio::time::sleep(IDLE_TIMEOUT);
    tokio::pin!(idle);
    'rtp_loop: loop {
        tokio::select! {
            biased;
            _ = read_cancel.cancelled() => break 'rtp_loop,
            result = rtp_reader.next_frame() => {
                idle.as_mut().reset(tokio::time::Instant::now() + IDLE_TIMEOUT);
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

                let Some(hdr) = livestream_codec::rtp::parse_rtp_header(&payload) else {
                    warn!(live_id = %live_id, "RTP header parse failed");
                    continue;
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

    lifecycle.disconnect();
    cancel.cancel();
    hub.remove_channel(&live_id);
    Ok(())
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::state::{SessionDescriptor, SessionEndpoint, SessionState};
    use livestream_core::types::Protocol;
    use rtsp_types::{Method, Request, Version, headers::CSeq};

    fn announce_session(uri: &str) -> RtspSession {
        let mut session = RtspSession::new();
        let req = Request::builder(Method::Announce, Version::V1_0)
            .typed_header(&CSeq::from(1u32))
            .request_uri(rtsp_types::Url::parse(uri).unwrap())
            .build(b"v=0\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\n".to_vec());
        session.handle_request(&req).unwrap();
        session
    }

    async fn register(registry: &SessionRegistry, id: &str, passphrase: Option<&str>) {
        let ct = CancellationToken::new();
        registry
            .register_session(
                Arc::new(tokio::sync::RwLock::new(SessionDescriptor {
                    id: id.to_string(),
                    protocol: Protocol::Rtsp,
                    endpoint: SessionEndpoint::new(None, passphrase.map(String::from)),
                    state: SessionState::Pending,
                })),
                ct.child_token(),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn passphrase_matches_enforces_opt_in() {
        let registry = Arc::new(SessionRegistry::new());
        register(&registry, "plain", None).await;
        register(&registry, "secure", Some("secret")).await;

        // 无 passphrase 的流：任意提供都通过
        let plain = announce_session("rtsp://example.com/live/plain");
        assert!(passphrase_matches(&registry, &plain).await);

        // 带 passphrase 的流：正确 → 通过；错误/缺失 → 拒绝
        let ok = announce_session("rtsp://secret@example.com/live/secure");
        assert!(passphrase_matches(&registry, &ok).await);
        let wrong = announce_session("rtsp://wrong@example.com/live/secure");
        assert!(!passphrase_matches(&registry, &wrong).await);
        let missing = announce_session("rtsp://example.com/live/secure");
        assert!(!passphrase_matches(&registry, &missing).await);

        // 未知流：不强制（无凭据可查）
        let unknown = announce_session("rtsp://example.com/live/nope");
        assert!(passphrase_matches(&registry, &unknown).await);
    }
}
