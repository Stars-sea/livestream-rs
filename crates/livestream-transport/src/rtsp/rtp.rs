//! RTP interleaved reader for RTSP TCP transport (RFC 2326 §10.12).
//!
//! Parses frames in the format: `$ | 1-byte channel | 2-byte length (BE) | payload`

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncReadExt, BufReader};

/// Reads RTP interleaved frames from an RTSP TCP connection.
///
/// Each frame is delimited by:
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

    /// Read the next interleaved frame. Returns `(channel, payload)`.
    pub async fn next_frame(&mut self) -> Result<(u8, Vec<u8>)> {
        let magic = self.reader.read_u8().await?;
        if magic != b'$' {
            return Ok((0, vec![magic]));
        }

        let channel = self.reader.read_u8().await?;
        let len = self.reader.read_u16().await? as usize;

        if len > self.buf.len() {
            self.buf.resize(len, 0);
        }
        self.reader.read_exact(&mut self.buf[..len]).await?;

        Ok((channel, self.buf[..len].to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_valid_interleaved_frame() {
        // $, channel=0, len=4, payload=[0xAA, 0xBB, 0xCC, 0xDD]
        let data: &[u8] = &[b'$', 0, 0, 4, 0xAA, 0xBB, 0xCC, 0xDD];
        let mut reader = RtpInterleavedReader::new(data);

        let (channel, payload) = reader.next_frame().await.unwrap();
        assert_eq!(channel, 0);
        assert_eq!(payload, vec![0xAA, 0xBB, 0xCC, 0xDD]);
    }

    #[tokio::test]
    async fn read_non_interleaved_byte_returns_fake_frame() {
        let data: &[u8] = b"T"; // stray RTSP text byte
        let mut reader = RtpInterleavedReader::new(data);

        let (channel, payload) = reader.next_frame().await.unwrap();
        assert_eq!(channel, 0);
        assert_eq!(payload, vec![b'T']);
    }

    #[tokio::test]
    async fn read_large_payload_resizes_buffer() {
        let len = 128u16;
        let mut data = vec![b'$', 0];
        data.extend_from_slice(&len.to_be_bytes());
        data.resize(4 + len as usize, 0x42);

        let mut reader = RtpInterleavedReader::new(data.as_slice());
        let (channel, payload) = reader.next_frame().await.unwrap();
        assert_eq!(channel, 0);
        assert_eq!(payload.len(), len as usize);
        assert!(payload.iter().all(|&b| b == 0x42));
    }
}
