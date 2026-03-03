use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use rml_rtmp::sessions::{ServerSession, ServerSessionResult};
use tokio::sync::mpsc;
use tracing::{Span, instrument};

use crate::ingest::events::StreamMessage;
use crate::core::output::FlvPacket;
use crate::server::contracts::StreamRegistry;

pub struct RtmpWorker {
    appname: String,
    stream_registry: Arc<dyn StreamRegistry>,
    flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
    stream_msg_tx: mpsc::UnboundedSender<(StreamMessage, Span)>,
    stream_muxers: HashMap<String, StreamMuxer>,
}

impl RtmpWorker {
    pub fn new(
        appname: String,
        stream_registry: Arc<dyn StreamRegistry>,
        flv_packet_tx: mpsc::UnboundedSender<FlvPacket>,
        stream_msg_tx: mpsc::UnboundedSender<(StreamMessage, Span)>,
    ) -> Self {
        Self {
            appname,
            stream_registry,
            flv_packet_tx,
            stream_msg_tx,
            stream_muxers: HashMap::new(),
        }
    }

    #[instrument(name = "ingest.rtmp_worker.publish_requested", skip(self, session), fields(stream.live_id = %stream_key, stream.app_name = %app_name, request.id = request_id))]
    pub async fn on_publish_requested(
        &mut self,
        session: &mut ServerSession,
        app_name: String,
        stream_key: String,
        request_id: u32,
    ) -> Result<Vec<ServerSessionResult>> {
        let accepted = app_name == self.appname && self.stream_registry.has_stream(&stream_key).await;

        if accepted {
            let mut muxer = StreamMuxer::new(stream_key.clone());
            self.send_stream_message(StreamMessage::ingest_worker_started(&stream_key));
            if let Some(header) = muxer.take_flv_header() {
                let _ = self.flv_packet_tx.send(FlvPacket::Data {
                    live_id: stream_key.clone(),
                    data: header,
                });
            }

            self.stream_muxers.insert(stream_key, muxer);
            return Ok(session.accept_request(request_id)?);
        }

        Ok(session.reject_request(request_id, "StreamNotFound", "Stream not found")?)
    }

    #[instrument(name = "ingest.rtmp_worker.publish_finished", skip(self), fields(stream.live_id = %stream_key))]
    pub fn on_publish_finished(&mut self, stream_key: String) {
        if let Some(muxer) = self.stream_muxers.remove(&stream_key) {
            if muxer.stream_started_notified {
                self.send_stream_message(StreamMessage::stream_stopped(&stream_key, None));
            }
            self.send_stream_message(StreamMessage::ingest_worker_stopped(&stream_key));
            let _ = self.flv_packet_tx.send(FlvPacket::EndOfStream {
                live_id: muxer.live_id,
            });
        }
    }

    #[instrument(name = "ingest.rtmp_worker.audio_data", skip(self, data), fields(stream.live_id = %stream_key, stream.timestamp = timestamp, media.bytes = data.len()))]
    pub fn on_audio_data(&mut self, stream_key: String, timestamp: u32, data: &[u8]) {
        let mut should_notify_started = false;
        if let Some(muxer) = self.stream_muxers.get_mut(&stream_key) {
            if !muxer.stream_started_notified {
                should_notify_started = true;
                muxer.stream_started_notified = true;
            }
            let tag = muxer.make_tag(8, timestamp, data);
            let _ = self.flv_packet_tx.send(FlvPacket::Data {
                live_id: muxer.live_id.clone(),
                data: tag,
            });
        }

        if should_notify_started {
            self.send_stream_message(StreamMessage::stream_started(&stream_key));
        }
    }

    #[instrument(name = "ingest.rtmp_worker.video_data", skip(self, data), fields(stream.live_id = %stream_key, stream.timestamp = timestamp, media.bytes = data.len()))]
    pub fn on_video_data(&mut self, stream_key: String, timestamp: u32, data: &[u8]) {
        let mut should_notify_started = false;
        if let Some(muxer) = self.stream_muxers.get_mut(&stream_key) {
            if !muxer.stream_started_notified {
                should_notify_started = true;
                muxer.stream_started_notified = true;
            }
            let tag = muxer.make_tag(9, timestamp, data);
            let _ = self.flv_packet_tx.send(FlvPacket::Data {
                live_id: muxer.live_id.clone(),
                data: tag,
            });
        }

        if should_notify_started {
            self.send_stream_message(StreamMessage::stream_started(&stream_key));
        }
    }

    #[instrument(name = "ingest.rtmp_worker.cleanup_disconnect", skip(self))]
    pub fn cleanup_disconnect(&mut self) {
        let tx = self.stream_msg_tx.clone();

        for (_, muxer) in self.stream_muxers.drain() {
            if muxer.stream_started_notified {
                let _ = tx.send((
                    StreamMessage::stream_stopped(&muxer.live_id, None),
                    Span::current(),
                ));
            }
            let _ = tx.send((
                StreamMessage::ingest_worker_stopped(&muxer.live_id),
                Span::current(),
            ));
            let _ = self.flv_packet_tx.send(FlvPacket::EndOfStream {
                live_id: muxer.live_id,
            });
        }
    }

    fn send_stream_message(&self, msg: StreamMessage) {
        let _ = self.stream_msg_tx.send((msg, Span::current()));
    }
}

struct StreamMuxer {
    live_id: String,
    header_sent: bool,
    stream_started_notified: bool,
}

impl StreamMuxer {
    fn new(live_id: String) -> Self {
        Self {
            live_id,
            header_sent: false,
            stream_started_notified: false,
        }
    }

    fn take_flv_header(&mut self) -> Option<bytes::Bytes> {
        if self.header_sent {
            return None;
        }

        self.header_sent = true;
        Some(bytes::Bytes::from_static(&[
            0x46, 0x4C, 0x56, 0x01, 0x05, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00, 0x00,
        ]))
    }

    fn make_tag(&self, tag_type: u8, timestamp: u32, payload: &[u8]) -> bytes::Bytes {
        let data_size = payload.len() as u32;
        let mut out = Vec::with_capacity(11 + payload.len() + 4);

        out.push(tag_type);
        out.push(((data_size >> 16) & 0xFF) as u8);
        out.push(((data_size >> 8) & 0xFF) as u8);
        out.push((data_size & 0xFF) as u8);

        out.push(((timestamp >> 16) & 0xFF) as u8);
        out.push(((timestamp >> 8) & 0xFF) as u8);
        out.push((timestamp & 0xFF) as u8);
        out.push(((timestamp >> 24) & 0xFF) as u8);

        out.extend_from_slice(&[0x00, 0x00, 0x00]);
        out.extend_from_slice(payload);

        let prev_size = 11 + data_size;
        out.push(((prev_size >> 24) & 0xFF) as u8);
        out.push(((prev_size >> 16) & 0xFF) as u8);
        out.push(((prev_size >> 8) & 0xFF) as u8);
        out.push((prev_size & 0xFF) as u8);

        bytes::Bytes::from(out)
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    use tokio::sync::mpsc;
    use tracing::Span;

    use super::{RtmpWorker, StreamMuxer};
    use crate::core::output::FlvPacket;
    use crate::ingest::events::StreamMessage;
    use crate::server::contracts::StreamRegistry;

    #[derive(Debug)]
    struct DummyRegistry;

    impl StreamRegistry for DummyRegistry {
        fn has_stream<'a>(
            &'a self,
            _live_id: &'a str,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            Box::pin(async move { true })
        }
    }

    fn build_worker() -> (
        RtmpWorker,
        mpsc::UnboundedReceiver<FlvPacket>,
        mpsc::UnboundedReceiver<(StreamMessage, Span)>,
    ) {
        let (flv_tx, flv_rx) = mpsc::unbounded_channel();
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let worker = RtmpWorker::new("lives".to_string(), Arc::new(DummyRegistry), flv_tx, msg_tx);
        (worker, flv_rx, msg_rx)
    }

    fn drain_messages(rx: &mut mpsc::UnboundedReceiver<(StreamMessage, Span)>) -> Vec<StreamMessage> {
        let mut out = Vec::new();
        while let Ok((msg, _)) = rx.try_recv() {
            out.push(msg);
        }
        out
    }

    fn drain_flv(rx: &mut mpsc::UnboundedReceiver<FlvPacket>) -> Vec<FlvPacket> {
        let mut out = Vec::new();
        while let Ok(packet) = rx.try_recv() {
            out.push(packet);
        }
        out
    }

    #[test]
    fn flv_header_only_emitted_once() {
        let mut muxer = StreamMuxer::new("live_1".to_string());

        let first = muxer.take_flv_header();
        let second = muxer.take_flv_header();

        assert!(first.is_some());
        assert!(second.is_none());
    }

    #[test]
    fn make_tag_builds_valid_flv_layout() {
        let muxer = StreamMuxer::new("live_1".to_string());
        let payload = [0x17u8, 0x01, 0x00, 0x00, 0x00];
        let tag = muxer.make_tag(9, 0x01020304, &payload);

        assert_eq!(tag[0], 9);

        let data_size = ((tag[1] as u32) << 16) | ((tag[2] as u32) << 8) | tag[3] as u32;
        assert_eq!(data_size as usize, payload.len());

        let timestamp_low = ((tag[4] as u32) << 16) | ((tag[5] as u32) << 8) | tag[6] as u32;
        let timestamp_ext = tag[7] as u32;
        let timestamp = (timestamp_ext << 24) | timestamp_low;
        assert_eq!(timestamp, 0x01020304);

        let payload_start = 11;
        let payload_end = payload_start + payload.len();
        assert_eq!(&tag[payload_start..payload_end], &payload);

        let prev_size_idx = payload_end;
        let prev_size = ((tag[prev_size_idx] as u32) << 24)
            | ((tag[prev_size_idx + 1] as u32) << 16)
            | ((tag[prev_size_idx + 2] as u32) << 8)
            | tag[prev_size_idx + 3] as u32;
        assert_eq!(prev_size, 11 + payload.len() as u32);
        assert_eq!(tag.len(), 11 + payload.len() + 4);
    }

    #[test]
    fn audio_data_notifies_stream_started_only_once() {
        let (mut worker, mut flv_rx, mut msg_rx) = build_worker();
        worker
            .stream_muxers
            .insert("live_1".to_string(), StreamMuxer::new("live_1".to_string()));

        worker.on_audio_data("live_1".to_string(), 100, &[0xAF, 0x01]);
        worker.on_audio_data("live_1".to_string(), 120, &[0xAF, 0x01]);

        let msgs = drain_messages(&mut msg_rx);
        let started_count = msgs
            .iter()
            .filter(|m| matches!(m, StreamMessage::StreamStarted { live_id } if live_id == "live_1"))
            .count();
        assert_eq!(started_count, 1);

        let packets = drain_flv(&mut flv_rx);
        let data_count = packets
            .iter()
            .filter(|p| matches!(p, FlvPacket::Data { live_id, .. } if live_id == "live_1"))
            .count();
        assert_eq!(data_count, 2);
    }

    #[test]
    fn publish_finished_emits_stop_events_and_eos() {
        let (mut worker, mut flv_rx, mut msg_rx) = build_worker();
        let mut muxer = StreamMuxer::new("live_2".to_string());
        muxer.stream_started_notified = true;
        worker.stream_muxers.insert("live_2".to_string(), muxer);

        worker.on_publish_finished("live_2".to_string());

        let msgs = drain_messages(&mut msg_rx);
        assert!(
            msgs.iter()
                .any(|m| matches!(m, StreamMessage::StreamStopped { live_id, error } if live_id == "live_2" && error.is_none()))
        );
        assert!(
            msgs.iter()
                .any(|m| matches!(m, StreamMessage::IngestWorkerStopped { live_id } if live_id == "live_2"))
        );

        let packets = drain_flv(&mut flv_rx);
        assert!(
            packets
                .iter()
                .any(|p| matches!(p, FlvPacket::EndOfStream { live_id } if live_id == "live_2"))
        );
    }

    #[test]
    fn cleanup_disconnect_emits_expected_events_per_stream() {
        let (mut worker, mut flv_rx, mut msg_rx) = build_worker();

        let mut started = StreamMuxer::new("live_started".to_string());
        started.stream_started_notified = true;
        worker
            .stream_muxers
            .insert("live_started".to_string(), started);

        let not_started = StreamMuxer::new("live_new".to_string());
        worker
            .stream_muxers
            .insert("live_new".to_string(), not_started);

        worker.cleanup_disconnect();

        let msgs = drain_messages(&mut msg_rx);
        assert!(
            msgs.iter().any(
                |m| matches!(m, StreamMessage::StreamStopped { live_id, .. } if live_id == "live_started")
            )
        );
        assert!(
            msgs.iter().any(
                |m| matches!(m, StreamMessage::IngestWorkerStopped { live_id } if live_id == "live_started")
            )
        );
        assert!(
            msgs.iter().any(
                |m| matches!(m, StreamMessage::IngestWorkerStopped { live_id } if live_id == "live_new")
            )
        );
        assert!(
            !msgs
                .iter()
                .any(|m| matches!(m, StreamMessage::StreamStopped { live_id, .. } if live_id == "live_new"))
        );

        let packets = drain_flv(&mut flv_rx);
        let eos_count = packets
            .iter()
            .filter(|p| matches!(p, FlvPacket::EndOfStream { .. }))
            .count();
        assert_eq!(eos_count, 2);
    }
}
