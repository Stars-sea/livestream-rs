//! RtmpSource — Source implementation for RTMP ingest.

use anyhow::Result;
use bytes::Bytes;
use livestream_codec::{EncodedPacket, NalData};
use livestream_core::{
    channel::SendError,
    pad::PadSender,
    traits::{Node, Source},
    types::{CodecParams, Protocol},
};
use tokio_util::sync::CancellationToken;

pub struct RtmpSource {
    stream_id: String,
    codec_params: Vec<CodecParams>,
    output_sender: PadSender<EncodedPacket>,
    frame_rx: std::sync::Mutex<Option<tokio::sync::mpsc::Receiver<RtmpRawFrame>>>,
    cancel: CancellationToken,
}

#[derive(Debug)]
pub struct RtmpRawFrame {
    pub data: Bytes,
    pub timestamp: u32,
    pub is_video: bool,
    pub is_audio: bool,
    pub is_script_data: bool,
}

impl RtmpSource {
    pub fn new(
        stream_id: &str,
        codec_params: Vec<CodecParams>,
        output_sender: PadSender<EncodedPacket>,
        cancel: CancellationToken,
    ) -> (Self, tokio::sync::mpsc::Sender<RtmpRawFrame>) {
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

impl Node for RtmpSource {
    fn name(&self) -> &str {
        "rtmp-source"
    }
}

#[async_trait::async_trait]
impl Source for RtmpSource {
    type Output = EncodedPacket;

    fn protocol(&self) -> Protocol {
        Protocol::Rtmp
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
            .ok_or_else(|| anyhow::anyhow!("RtmpSource already started"))?;

        tracing::info!(stream = %self.stream_id, "RtmpSource started");

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

                    let Some(pkt) = convert_frame(frame) else {
                        continue;
                    };

                    // Retry on backpressure — transient channel-full should not
                    // kill the source. Only break if the receiver is gone.
                    loop {
                        match self.output_sender.send(pkt.clone()) {
                            Ok(()) => break,
                            Err(SendError::Closed) => {
                                tracing::debug!(stream = %self.stream_id, "Output receiver closed");
                                self.cancel.cancel();
                                return Ok(());
                            }
                            Err(SendError::Full) => {
                                // Channel full — yield and retry.
                                tokio::task::yield_now().await;
                            }
                        }
                    }
                }
                _ = self.cancel.cancelled() => {
                    tracing::info!(stream = %self.stream_id, "RtmpSource cancelled");
                    break;
                }
            }
        }

        self.cancel.cancel();
        tracing::info!(stream = %self.stream_id, "RtmpSource stopped");
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        self.cancel.cancel();
        Ok(())
    }
}

/// Convert AVCC (4-byte length-prefixed NAL units) to Annex B
/// (00 00 00 01 start-code delimited NAL units).
///
/// FLV video tags carry AVCC-format data. The pipeline's internal convention
/// is Annex B (same as what RTSP/RTP depacketizer produces), so we convert at
/// the source boundary. `FlvMux::annex_b_to_avcc()` will convert back when
/// muxing FLV output.
fn avcc_to_annex_b(data: &[u8]) -> Bytes {
    let mut out = bytes::BytesMut::with_capacity(data.len() + data.len() / 2);
    let mut pos = 0;
    while pos + 4 <= data.len() {
        let nal_len =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;
        if pos + nal_len > data.len() {
            break;
        }
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        out.extend_from_slice(&data[pos..pos + nal_len]);
        pos += nal_len;
    }
    out.freeze()
}
/// Convert a raw RTMP frame into an EncodedPacket.
fn convert_frame(frame: RtmpRawFrame) -> Option<EncodedPacket> {
    if frame.is_script_data || frame.data.is_empty() {
        return None;
    }

    let pts_ms = frame.timestamp as i64;

    if frame.is_video && frame.data.len() >= 2 {
        let first_byte = frame.data[0];
        let frame_type = (first_byte >> 4) & 0x0F; // 1=keyframe, 2=inter, 3=disposable
        let codec_id = first_byte & 0x0F; // 7=H264, 12=H265
        let avc_packet_type = frame.data[1];

        let codec = match codec_id {
            7 => livestream_codec::Codec::H264,
            12 => livestream_codec::Codec::H265,
            _ => {
                tracing::warn!(codec_id, "Unsupported FLV video codec");
                return None;
            }
        };

        let is_keyframe = frame_type == 1;
        let is_seq_header = avc_packet_type == 0;

        // rml_rtmp gives us the full FLV tag body (5-byte header + codec data).
        // Strip the header: FlvMux will re-add it.
        let raw_codec_data = if frame.data.len() > 5 {
            frame.data.slice(5..)
        } else {
            Bytes::new()
        };

        // Convert AVCC → Annex B for NAL unit packets. Sequence headers
        // carry AVCDecoderConfigurationRecord (used as extradata), not NAL units.
        let codec_data = if !is_seq_header && !raw_codec_data.is_empty() {
            avcc_to_annex_b(&raw_codec_data)
        } else {
            raw_codec_data
        };

        // For sequence headers, populate extradata with AVCDecoderConfigurationRecord
        // so build_avc_sequence_header can use it.
        let extradata = if is_seq_header && !codec_data.is_empty() {
            Some(codec_data.clone())
        } else {
            None
        };

        // CTS = Composition Time offset (PTS - DTS). FLV tag body bytes 2-4.
        let cts_ms = if frame.data.len() >= 5 {
            let cts_raw = ((frame.data[2] as i32) << 16)
                | ((frame.data[3] as i32) << 8)
                | (frame.data[4] as i32);
            if cts_raw & 0x800000 != 0 {
                cts_raw | !0xFFFFFF
            } else {
                cts_raw
            }
        } else {
            0
        };
        let dts_ms = pts_ms - cts_ms as i64;

        Some(EncodedPacket {
            codec,
            stream_index: 0,
            data: NalData::AnnexB(codec_data),
            pts_ms: Some(pts_ms),
            dts_ms: Some(dts_ms),
            is_keyframe,
            is_sequence_header: is_seq_header,
            is_script_data: false,
            extradata,
        })
    } else if frame.is_audio {
        let is_aac_seq = frame.data.len() >= 2
            && (frame.data[0] >> 4) == 10 // SoundFormat = AAC
            && frame.data[1] == 0; // AACPacketType = sequence header
        // Strip 2-byte FLV audio tag header (SoundFormat + AACPacketType).
        // FlvMux will re-add it.
        let codec_data = if frame.data.len() > 2 {
            frame.data.slice(2..)
        } else {
            Bytes::new()
        };
        Some(EncodedPacket {
            codec: livestream_codec::Codec::Aac,
            stream_index: 0,
            data: NalData::AnnexB(codec_data),
            pts_ms: Some(pts_ms),
            dts_ms: None,
            is_keyframe: false,
            is_sequence_header: is_aac_seq,
            is_script_data: false,
            extradata: None,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use livestream_codec::MediaPacket;

    #[test]
    fn source_protocol_is_rtmp() {
        let (tx, _rx) = PadSender::<EncodedPacket>::new_channel(4);
        let (src, _frame_tx) = RtmpSource::new("test", vec![], tx, CancellationToken::new());
        assert_eq!(src.protocol(), Protocol::Rtmp);
        assert_eq!(src.name(), "rtmp-source");
    }

    #[test]
    fn start_consumes_frame_rx() {
        let (tx, _rx) = PadSender::<EncodedPacket>::new_channel(4);
        let cancel = CancellationToken::new();
        let (src, _frame_tx) = RtmpSource::new("test", vec![], tx, cancel.clone());

        cancel.cancel();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(src.start());
        assert!(result.is_ok());

        let result = rt.block_on(src.start());
        assert!(result.is_err());
    }

    #[test]
    fn script_data_frames_are_skipped() {
        let frame = RtmpRawFrame {
            data: Bytes::from_static(b"onMetaData..."),
            timestamp: 0,
            is_video: false,
            is_audio: false,
            is_script_data: true,
        };
        assert!(convert_frame(frame).is_none());
    }

    #[test]
    fn empty_frames_are_skipped() {
        let frame = RtmpRawFrame {
            data: Bytes::new(),
            timestamp: 0,
            is_video: true,
            is_audio: false,
            is_script_data: false,
        };
        assert!(convert_frame(frame).is_none());
    }

    #[test]
    fn video_keyframe_conversion() {
        // 0x17: frame_type=1 (keyframe), codec_id=7 (H264), avc_packet_type=0 (seq header)
        let frame = RtmpRawFrame {
            data: Bytes::from_static(&[0x17, 0x00, 0x00, 0x00, 0x00]),
            timestamp: 100,
            is_video: true,
            is_audio: false,
            is_script_data: false,
        };
        let pkt = convert_frame(frame).unwrap();
        assert!(pkt.is_keyframe());
        assert!(pkt.is_sequence_header);
        assert_eq!(pkt.timestamp(), Some(std::time::Duration::from_millis(100)));
    }

    #[test]
    fn video_inter_frame_with_cts() {
        // 0x27: frame_type=2 (inter), codec_id=7 (H264), avc_packet_type=1 (NALU)
        // CTS = 0x000021 = 33ms
        let frame = RtmpRawFrame {
            data: Bytes::from_static(&[0x27, 0x01, 0x00, 0x00, 0x21]),
            timestamp: 1000,
            is_video: true,
            is_audio: false,
            is_script_data: false,
        };
        let pkt = convert_frame(frame).unwrap();
        assert!(!pkt.is_keyframe());
        assert!(!pkt.is_sequence_header);
        assert_eq!(pkt.pts_ms, Some(1000));
        // Non-keyframe: timeline starts at 0 + cts, PTS = DTS + CTS
        // This frame: PTS=1000, CTS=33, so DTS=967
        assert_eq!(pkt.dts_ms, Some(967));
    }
}
