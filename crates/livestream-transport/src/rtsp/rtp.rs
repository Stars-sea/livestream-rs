//! RTP interleaved reader for RTSP TCP transport (RFC 2326 §10.12).
//!
//! Parses frames in the format: `$ | 1-byte channel | 2-byte length (BE) | payload`
//!
//! RTSP permits clients to send requests (TEARDOWN, OPTIONS, …) on the same
//! TCP connection while a session is recording (§10.12). Interleaved data is
//! always `$`-prefixed, so a leading text byte is either the start of an
//! in-band request or stray data from a corrupted stream — both are handled
//! here.

use anyhow::{Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt, BufReader};

/// Maximum size of an in-band RTSP request (request line + headers + body).
const MAX_INBAND_REQUEST_SIZE: usize = 64 * 1024;

/// Maximum length of an RTSP request line. Real request lines (method + URI +
/// version) are a few hundred bytes; anything longer cannot be a request and
/// is treated as stray data without unbounded buffering.
const MAX_REQUEST_LINE_SIZE: usize = 4096;

/// One unit read from an RTSP connection while a session is recording.
#[derive(Debug)]
pub enum RtpReadItem {
    /// A complete interleaved RTP frame: `$ channel len payload`.
    Interleaved { channel: u8, payload: Vec<u8> },
    /// An RTSP request received in-band on the same connection.
    RtspRequest(Vec<u8>),
    /// Non-interleaved bytes that do not form an RTSP request (stray data).
    Stray(Vec<u8>),
}

/// Result of reading a bounded line of non-interleaved bytes.
enum LineRead {
    /// The line is a well-formed RTSP request line (the full line included).
    Request(Vec<u8>),
    /// The bytes are not a request line (stray data, up to the bound).
    Stray(Vec<u8>),
}

/// Reads RTP interleaved frames and in-band RTSP requests from an RTSP TCP
/// connection.
///
/// Each interleaved frame is delimited by:
/// - Magic byte `$` (0x24)
/// - 1-byte channel identifier
/// - 2-byte payload length (big-endian)
/// - `length` bytes of payload
pub struct RtpInterleavedReader<R: AsyncRead + Unpin> {
    reader: BufReader<R>,
    buf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> RtpInterleavedReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
            buf: vec![0u8; 65536],
        }
    }

    /// Read the next item: an interleaved RTP frame, an in-band RTSP
    /// request, or stray bytes. Returns an error on EOF or when the
    /// connection is unusable.
    pub async fn next_item(&mut self) -> Result<RtpReadItem> {
        let first = self.reader.read_u8().await?;
        if first == b'$' {
            let channel = self.reader.read_u8().await?;
            let payload = self.read_frame_payload().await?;
            return Ok(RtpReadItem::Interleaved { channel, payload });
        }

        // Leading non-`$` byte: possibly the start of an in-band RTSP
        // request (RFC 2326 §10.12) or stray data from a corrupted stream.
        match self.read_request_line(first).await? {
            LineRead::Request(line) => {
                let request = self.read_request(line).await?;
                Ok(RtpReadItem::RtspRequest(request))
            }
            LineRead::Stray(bytes) => Ok(RtpReadItem::Stray(bytes)),
        }
    }

    /// Read one bounded line starting with `first`. Only a well-formed
    /// request line (METHOD SP URI SP RTSP/x.y) is classified as `Request`;
    /// anything else is `Stray`.
    async fn read_request_line(&mut self, first: u8) -> Result<LineRead> {
        let mut bytes = vec![first];
        while bytes.len() < MAX_REQUEST_LINE_SIZE {
            let b = self.reader.read_u8().await?;
            bytes.push(b);
            if b != b'\n' {
                continue;
            }
            if is_rtsp_request_line(&bytes) {
                return Ok(LineRead::Request(bytes));
            }
            break;
        }
        Ok(LineRead::Stray(bytes))
    }

    async fn read_frame_payload(&mut self) -> Result<Vec<u8>> {
        let len = self.reader.read_u16().await? as usize;
        if len > self.buf.len() {
            self.buf.resize(len, 0);
        }
        self.reader.read_exact(&mut self.buf[..len]).await?;
        Ok(self.buf[..len].to_vec())
    }

    /// After a valid request line, read headers (through `\r\n\r\n`) and the
    /// Content-Length body.
    async fn read_request(&mut self, mut request: Vec<u8>) -> Result<Vec<u8>> {
        while !request.ends_with(b"\r\n\r\n") {
            if request.len() >= MAX_INBAND_REQUEST_SIZE {
                bail!(
                    "RTSP in-band request exceeds {} bytes",
                    MAX_INBAND_REQUEST_SIZE
                );
            }
            let b = self.reader.read_u8().await?;
            request.push(b);
        }
        if let Some(len) = content_length(&request) {
            if request.len().saturating_add(len) > MAX_INBAND_REQUEST_SIZE {
                bail!(
                    "RTSP in-band request body exceeds {} bytes",
                    MAX_INBAND_REQUEST_SIZE
                );
            }
            let mut body = vec![0u8; len];
            self.reader.read_exact(&mut body).await?;
            request.extend_from_slice(&body);
        }
        Ok(request)
    }
}

/// True when `line` (terminated by `\n`, optionally `\r\n`) is an RTSP
/// request line: `METHOD SP URI SP RTSP/x.y`.
fn is_rtsp_request_line(line: &[u8]) -> bool {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let mut parts = line.split(|&b| b == b' ');
    let Some(method) = parts.next() else {
        return false;
    };
    let Some(uri) = parts.next() else {
        return false;
    };
    let Some(version) = parts.next() else {
        return false;
    };
    if parts.next().is_some() || method.is_empty() || uri.is_empty() {
        return false;
    }
    // Standard RTSP methods are uppercase ASCII tokens (GET_PARAMETER,
    // SET_PARAMETER contain underscores).
    if !method
        .iter()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_')
    {
        return false;
    }
    version.starts_with(b"RTSP/")
}

fn content_length(headers: &[u8]) -> Option<usize> {
    let headers = String::from_utf8_lossy(headers);
    headers.lines().find_map(|line| {
        if line.trim().to_lowercase().starts_with("content-length:") {
            line.split(':').nth(1).and_then(|v| v.trim().parse().ok())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_valid_interleaved_frame() {
        // $, channel=0, len=4, payload=[0xAA, 0xBB, 0xCC, 0xDD]
        let data: &[u8] = &[b'$', 0, 0, 4, 0xAA, 0xBB, 0xCC, 0xDD];
        let mut reader = RtpInterleavedReader::new(data);

        match reader.next_item().await.unwrap() {
            RtpReadItem::Interleaved { channel, payload } => {
                assert_eq!(channel, 0);
                assert_eq!(payload, vec![0xAA, 0xBB, 0xCC, 0xDD]);
            }
            other => panic!("expected Interleaved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_large_payload_resizes_buffer() {
        let len = 128u16;
        let mut data = vec![b'$', 0];
        data.extend_from_slice(&len.to_be_bytes());
        data.resize(4 + len as usize, 0x42);

        let mut reader = RtpInterleavedReader::new(data.as_slice());
        match reader.next_item().await.unwrap() {
            RtpReadItem::Interleaved { channel, payload } => {
                assert_eq!(channel, 0);
                assert_eq!(payload.len(), len as usize);
                assert!(payload.iter().all(|&b| b == 0x42));
            }
            other => panic!("expected Interleaved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_inband_rtsp_request() {
        let data: &[u8] =
            b"TEARDOWN rtsp://example.com/live/abc RTSP/1.0\r\nCSeq: 5\r\nSession: abc\r\n\r\n";
        let mut reader = RtpInterleavedReader::new(data);

        match reader.next_item().await.unwrap() {
            RtpReadItem::RtspRequest(req) => assert_eq!(req, data),
            other => panic!("expected RtspRequest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_inband_request_with_body() {
        let data: &[u8] = b"GET_PARAMETER rtsp://example.com/live/abc RTSP/1.0\r\n\
CSeq: 6\r\nContent-Length: 4\r\n\r\nABCD";
        let mut reader = RtpInterleavedReader::new(data);

        match reader.next_item().await.unwrap() {
            RtpReadItem::RtspRequest(req) => assert_eq!(req, data),
            other => panic!("expected RtspRequest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn inband_request_then_interleaved_frame() {
        let request: &[u8] = b"TEARDOWN rtsp://example.com/live/abc RTSP/1.0\r\nCSeq: 5\r\n\r\n";
        let frame: &[u8] = &[b'$', 1, 0, 2, 0xAA, 0xBB];
        let mut data = request.to_vec();
        data.extend_from_slice(frame);

        let mut reader = RtpInterleavedReader::new(data.as_slice());
        match reader.next_item().await.unwrap() {
            RtpReadItem::RtspRequest(req) => assert_eq!(req, request),
            other => panic!("expected RtspRequest, got {other:?}"),
        }
        match reader.next_item().await.unwrap() {
            RtpReadItem::Interleaved { channel, payload } => {
                assert_eq!(channel, 1);
                assert_eq!(payload, vec![0xAA, 0xBB]);
            }
            other => panic!("expected Interleaved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stray_text_line_is_reported_not_parsed_as_frame() {
        let data: &[u8] = b"T\r\n"; // stray RTSP text bytes
        let mut reader = RtpInterleavedReader::new(data);

        match reader.next_item().await.unwrap() {
            RtpReadItem::Stray(bytes) => assert_eq!(bytes, b"T\r\n"),
            other => panic!("expected Stray, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stray_binary_without_newline_is_bounded() {
        let data = vec![0xAAu8; MAX_REQUEST_LINE_SIZE];
        let mut reader = RtpInterleavedReader::new(data.as_slice());

        match reader.next_item().await.unwrap() {
            RtpReadItem::Stray(bytes) => assert_eq!(bytes.len(), MAX_REQUEST_LINE_SIZE),
            other => panic!("expected Stray, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn oversized_inband_request_is_rejected() {
        let mut data = b"TEARDOWN rtsp://example.com/live/abc RTSP/1.0\r\n".to_vec();
        data.extend(vec![b'X'; MAX_INBAND_REQUEST_SIZE]);

        let mut reader = RtpInterleavedReader::new(data.as_slice());
        assert!(reader.next_item().await.is_err());
    }
}
