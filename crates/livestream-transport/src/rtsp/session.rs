//! RTSP session handler using `rtsp-types` for message parsing and serialization.
//!
//! State machine: WaitAnnounce → WaitSetup → WaitRecord → Recording → Teardown.

use anyhow::Result;
use rtsp_types::{
    Empty, Message, Method, Request, Response, StatusCode, Version,
    headers::{CSeq, TRANSPORT},
};
use tracing::info;

use super::sdp::{self, ParsedSdp};

#[derive(Debug, PartialEq)]
enum State {
    WaitAnnounce,
    WaitSetup,
    WaitRecord,
    Recording,
    Teardown,
}

/// RTSP session handler managing the ANNOUNCE → SETUP → RECORD → TEARDOWN lifecycle.
///
/// Stores the parsed SDP after ANNOUNCE and tracks channel assignments from SETUP.
pub struct RtspSession {
    state: State,
    parsed_sdp: Option<ParsedSdp>,
    video_channel: u8,
    audio_channel: u8,
}

impl Default for RtspSession {
    fn default() -> Self {
        Self::new()
    }
}

impl RtspSession {
    pub fn new() -> Self {
        Self {
            state: State::WaitAnnounce,
            parsed_sdp: None,
            video_channel: 0,
            audio_channel: 2,
        }
    }

    /// Handle a parsed RTSP request. Returns an optional response.
    pub fn handle_request(
        &mut self,
        request: &Request<Vec<u8>>,
    ) -> Result<Option<Response<Empty>>> {
        let cseq = request
            .typed_header::<CSeq>()
            .ok()
            .flatten()
            .map(|c| *c)
            .unwrap_or(0);

        match request.method() {
            Method::Announce => {
                if self.state != State::WaitAnnounce {
                    return Ok(Some(error_response(
                        StatusCode::MethodNotValidInThisState,
                        cseq,
                    )));
                }
                let body = String::from_utf8_lossy(request.body());
                let parsed = sdp::parse_sdp(&body)?;
                self.parsed_sdp = Some(parsed);
                self.state = State::WaitSetup;
                info!("RTSP ANNOUNCE received, SDP parsed");
                Ok(Some(ok_response(cseq)))
            }
            Method::Options => Ok(Some(ok_response(cseq))),
            Method::Setup => {
                if self.state != State::WaitSetup && self.state != State::WaitRecord {
                    return Ok(Some(error_response(
                        StatusCode::MethodNotValidInThisState,
                        cseq,
                    )));
                }
                let is_video = request
                    .request_uri()
                    .map(|u| u.path().contains("track1") || !u.path().contains("track2"))
                    .unwrap_or(true);

                let (ch, _rtcp_ch) = if is_video {
                    (self.video_channel, self.video_channel + 1)
                } else {
                    (self.audio_channel, self.audio_channel + 1)
                };
                self.state = State::WaitRecord;

                Ok(Some(
                    Response::builder(Version::V1_0, StatusCode::Ok)
                        .typed_header(&CSeq::from(cseq))
                        .header(
                            TRANSPORT,
                            format!("RTP/AVP/TCP;interleaved={}-{}", ch, ch + 1),
                        )
                        .empty(),
                ))
            }
            Method::Record => {
                if self.state != State::WaitRecord {
                    return Ok(Some(error_response(
                        StatusCode::MethodNotValidInThisState,
                        cseq,
                    )));
                }
                self.state = State::Recording;
                Ok(Some(ok_response(cseq)))
            }
            Method::Teardown => {
                self.state = State::Teardown;
                Ok(Some(ok_response(cseq)))
            }
            _ => Ok(Some(error_response(StatusCode::MethodNotAllowed, cseq))),
        }
    }

    pub fn is_teardown(&self) -> bool {
        self.state == State::Teardown
    }

    pub fn is_recording(&self) -> bool {
        self.state == State::Recording
    }

    /// Raw SDP body for feeding to FFmpeg's RTP demuxer.
    pub fn sdp_body(&self) -> Option<&str> {
        self.parsed_sdp.as_ref().map(|p| p.raw_sdp())
    }

    /// Codec parameters extracted from the SDP.
    pub fn codec_params(&self) -> Option<&[livestream_core::types::CodecParams]> {
        self.parsed_sdp.as_ref().map(|p| p.codec_params.as_slice())
    }

    pub fn video_channel(&self) -> u8 {
        self.video_channel
    }

    pub fn audio_channel(&self) -> u8 {
        self.audio_channel
    }
}

fn ok_response(cseq: u32) -> Response<Empty> {
    Response::builder(Version::V1_0, StatusCode::Ok)
        .typed_header(&CSeq::from(cseq))
        .empty()
}

pub fn error_response(status: StatusCode, cseq: u32) -> Response<Empty> {
    Response::builder(Version::V1_0, status)
        .typed_header(&CSeq::from(cseq))
        .empty()
}

/// Parse an RTSP message from raw bytes.
pub fn parse_message(data: &[u8]) -> Result<Message<Vec<u8>>> {
    let (msg, _consumed) =
        Message::parse(data).map_err(|e| anyhow::anyhow!("RTSP parse error: {:?}", e))?;
    Ok(msg)
}

/// Serialize an RTSP response to wire bytes.
pub fn serialize_response(response: &Response<Empty>) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    response
        .write(&mut buf)
        .map_err(|e| anyhow::anyhow!("RTSP write error: {}", e))?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_transitions_to_setup() {
        let mut session = RtspSession::new();
        let req = Request::builder(Method::Announce, Version::V1_0)
            .typed_header(&CSeq::from(1u32))
            .build(b"v=0\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\n".to_vec());
        let resp = session.handle_request(&req).unwrap();
        assert!(resp.is_some());
        assert!(!session.is_recording());
        assert!(session.sdp_body().is_some());
        assert!(session.codec_params().is_some());
    }

    #[test]
    fn full_handshake() {
        let mut session = RtspSession::new();
        let announce = Request::builder(Method::Announce, Version::V1_0)
            .typed_header(&CSeq::from(1u32))
            .build(b"v=0\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\n".to_vec());
        session.handle_request(&announce).unwrap();

        let setup = Request::builder(Method::Setup, Version::V1_0)
            .request_uri(rtsp_types::Url::parse("rtsp://example.com/live/track1").unwrap())
            .typed_header(&CSeq::from(2u32))
            .build(b"".to_vec());
        session.handle_request(&setup).unwrap();

        let record = Request::builder(Method::Record, Version::V1_0)
            .typed_header(&CSeq::from(3u32))
            .build(b"".to_vec());
        let resp = session.handle_request(&record).unwrap();
        assert!(resp.is_some());
        assert!(session.is_recording());
    }

    #[test]
    fn codec_params_available_after_announce() {
        let mut session = RtspSession::new();
        let req = Request::builder(Method::Announce, Version::V1_0)
            .typed_header(&CSeq::from(1u32))
            .build(
                b"v=0\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\n\
                  m=audio 0 RTP/AVP 97\r\na=rtpmap:97 mpeg4-generic/44100\r\n"
                    .to_vec(),
            );
        session.handle_request(&req).unwrap();

        let params = session.codec_params().unwrap();
        assert_eq!(params.len(), 2);
    }
}
