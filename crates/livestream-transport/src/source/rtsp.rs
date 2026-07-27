//! RtspSource — Source implementation for RTSP ingest.
//!
//! Receives RTP interleaved frames from the connection handler via an internal
//! mpsc channel, parses the RTP header, and forwards complete `RtpPacket`s to
//! the pipeline through its output `PadSender`.

use anyhow::Result;
use livestream_codec::RtpPacket;
use livestream_core::{
    pad::PadSender,
    traits::{Node, Source},
    types::{CodecParams, Protocol},
};
use tokio_util::sync::CancellationToken;

/// RTSP ingest source.
///
/// `Output = RtpPacket` — the source produces complete RTP packets
/// (12-byte header + payload). Downstream `RtpDepackProcessor` feeds
/// these to FFmpeg's RTP demuxer for depacketization.
pub struct RtspSource {
    stream_id: String,
    codec_params: Vec<CodecParams>,
    output_sender: PadSender<RtpPacket>,
    /// Receives (channel, rtp_timestamp, marker, raw_payload) from the
    /// connection handler. Wrapped in `Mutex<Option<>>` so that `start()`
    /// can take ownership (via `&self`).
    frame_rx: std::sync::Mutex<Option<tokio::sync::mpsc::Receiver<RawRtpFrame>>>,
    cancel: CancellationToken,
}

/// A raw RTP frame as received from the interleaved reader.
///
/// `payload` contains the **complete** RTP data (12-byte header + payload).
/// The source reconstructs the `RtpPacket` before sending it downstream.
#[derive(Debug)]
pub struct RawRtpFrame {
    pub channel: u8,
    pub payload_type: u8,
    pub rtp_timestamp: u32,
    pub marker: bool,
    pub sequence_number: u16,
    pub ssrc: u32,
    /// Complete RTP data (12-byte header + payload).
    pub rtp_data: Vec<u8>,
}

impl RtspSource {
    /// Create a new RTSP source and the corresponding frame sender.
    ///
    /// Returns `(source, frame_tx)` — the `frame_tx` is passed to the
    /// connection handler which feeds raw RTP frames into the source.
    pub fn new(
        stream_id: &str,
        codec_params: Vec<CodecParams>,
        output_sender: PadSender<RtpPacket>,
        cancel: CancellationToken,
    ) -> (Self, tokio::sync::mpsc::Sender<RawRtpFrame>) {
        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel(256);
        let source = Self {
            stream_id: stream_id.into(),
            codec_params,
            output_sender,
            frame_rx: std::sync::Mutex::new(Some(frame_rx)),
            cancel,
        };
        (source, frame_tx)
    }
}

impl Node for RtspSource {
    fn name(&self) -> &str {
        "rtsp-source"
    }
}

#[async_trait::async_trait]
impl Source for RtspSource {
    type Output = RtpPacket;

    fn protocol(&self) -> Protocol {
        Protocol::Rtsp
    }

    fn codec_params(&self) -> &[CodecParams] {
        &self.codec_params
    }

    fn output(&self) -> &PadSender<Self::Output> {
        &self.output_sender
    }

    async fn start(&self) -> Result<()> {
        let mut rx = self
            .frame_rx
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| anyhow::anyhow!("RtspSource already started"))?;

        tracing::info!(stream = %self.stream_id, "RtspSource started");

        loop {
            tokio::select! {
                frame = rx.recv() => {
                    let frame = match frame {
                        Some(f) => f,
                        None => {
                            tracing::info!(stream = %self.stream_id, "Frame sender closed");
                            break;
                        }
                    };

                    let rtp_pkt = RtpPacket {
                        payload_type: frame.payload_type,
                        channel: frame.channel,
                        rtp_timestamp: frame.rtp_timestamp,
                        marker: frame.marker,
                        data: bytes::Bytes::from(frame.rtp_data),
                    };

                    // Retry on backpressure — only break if receiver is gone.
                    loop {
                        match self.output_sender.send(rtp_pkt.clone()) {
                            Ok(()) => break,
                            Err(livestream_core::channel::SendError::Closed) => {
                                tracing::debug!(stream = %self.stream_id, "Output receiver closed");
                                break;
                            }
                            Err(livestream_core::channel::SendError::Full) => {
                                tokio::task::yield_now().await;
                            }
                        }
                    }
                }
                _ = self.cancel.cancelled() => {
                    tracing::info!(stream = %self.stream_id, "RtspSource cancelled");
                    break;
                }
            }
        }

        tracing::info!(stream = %self.stream_id, "RtspSource stopped");
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        self.cancel.cancel();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_protocol_is_rtsp() {
        let (tx, _rx) = PadSender::<RtpPacket>::new_channel(4);
        let (src, _frame_tx) = RtspSource::new("test", vec![], tx, CancellationToken::new());
        assert_eq!(src.protocol(), Protocol::Rtsp);
        assert_eq!(src.name(), "rtsp-source");
    }

    #[test]
    fn start_consumes_frame_rx() {
        let (tx, _rx) = PadSender::<RtpPacket>::new_channel(4);
        let cancel = CancellationToken::new();
        let (src, _frame_tx) = RtspSource::new("test", vec![], tx, cancel.clone());

        // First start should succeed.
        cancel.cancel(); // cancel so start() returns immediately
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(src.start());
        assert!(result.is_ok());

        // Second start should fail (frame_rx already taken).
        let result = rt.block_on(src.start());
        assert!(result.is_err());
    }
}
