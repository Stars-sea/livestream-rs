use std::sync::Arc;

use anyhow::Result;
use bytes::{Bytes, BytesMut};
use rml_rtmp::chunk_io::Packet;
use rml_rtmp::sessions::{ServerSession, ServerSessionResult};
use rml_rtmp::time::RtmpTimestamp;
use tokio::sync::RwLock;

use crate::contracts::StreamRegistry;
use crate::egress::broadcaster::BroadcastRx;
use crate::egress::dispatcher::{StreamDispatcher, StreamState};
use crate::media::flv_parser::FlvTag;
use crate::telemetry::metrics;

pub struct RtmpEgressHandler {
    dispatcher: StreamDispatcher,
    stream_registry: Arc<dyn StreamRegistry>,
    active_stream_rx: Option<BroadcastRx<Arc<FlvTag>>>,
    current_stream_id: u32,
    stream_state: Option<Arc<RwLock<StreamState>>>,
    sent_headers: bool,
    pull_guard: Option<metrics::MetricGuard>,
}

impl RtmpEgressHandler {
    pub fn new(dispatcher: StreamDispatcher, stream_registry: Arc<dyn StreamRegistry>) -> Self {
        Self {
            dispatcher,
            stream_registry,
            active_stream_rx: None,
            current_stream_id: 0,
            stream_state: None,
            sent_headers: false,
            pull_guard: None,
        }
    }

    pub async fn on_play_requested(
        &mut self,
        session: &mut ServerSession,
        stream_key: String,
        request_id: u32,
        stream_id: u32,
    ) -> Result<Vec<ServerSessionResult>> {
        if self.stream_registry.get_stream(&stream_key).await.is_some() {
            self.current_stream_id = stream_id;

            let (rx, state) = self.dispatcher.subscribe(&stream_key);
            self.active_stream_rx = Some(rx);
            self.stream_state = Some(state);
            self.sent_headers = false;

            if self.pull_guard.is_none() {
                self.pull_guard = Some(metrics::MetricGuard::new(
                    &metrics::get_metrics().egress_connections,
                    vec![],
                ));
            }

            return Ok(session.accept_request(request_id)?);
        }

        Ok(session.reject_request(request_id, "StreamNotFound", "Stream not found")?)
    }

    pub fn on_play_finished(&mut self) {
        self.active_stream_rx = None;
        self.stream_state = None;
        self.sent_headers = false;
    }

    pub async fn wait_for_tag(&mut self) -> Result<Arc<FlvTag>> {
        match &mut self.active_stream_rx {
            Some(rx) => Ok(rx.recv().await?),
            None => std::future::pending().await,
        }
    }

    pub async fn handle_broadcast_tag(
        &mut self,
        session: &mut ServerSession,
        tag: Arc<FlvTag>,
    ) -> Result<Bytes> {
        let mut bytes_out = BytesMut::new();

        if !self.sent_headers {
            for packet in self.send_cached_headers(session).await? {
                bytes_out.extend_from_slice(&packet.bytes);
            }

            self.sent_headers = true;
        }

        if let Some(packet) = self.send_tag(session, tag.as_ref()).await? {
            bytes_out.extend_from_slice(&packet.bytes);
        }

        Ok(bytes_out.freeze())
    }

    async fn send_cached_headers(&mut self, session: &mut ServerSession) -> Result<Vec<Packet>> {
        let Some(state) = self.stream_state.as_ref() else {
            return Ok(Vec::new());
        };

        let cached_tags = state.read().await.cached_headers();

        let mut packets = Vec::with_capacity(cached_tags.len());
        for tag in cached_tags {
            if let Some(packet) = self.send_tag(session, tag.as_ref()).await? {
                packets.push(packet);
            }
        }

        Ok(packets)
    }

    async fn send_tag(
        &mut self,
        session: &mut ServerSession,
        tag: &FlvTag,
    ) -> Result<Option<Packet>> {
        let stream_id = self.current_stream_id;
        let packet = match tag {
            FlvTag::Audio { timestamp, payload } => session.send_audio_data(
                stream_id,
                payload.clone().into(),
                RtmpTimestamp::new(*timestamp),
                false,
            )?,
            FlvTag::Video {
                timestamp,
                payload,
                is_keyframe,
            } => session.send_video_data(
                stream_id,
                payload.clone().into(),
                RtmpTimestamp::new(*timestamp),
                !*is_keyframe,
            )?,
            FlvTag::ScriptData { .. } => return Ok(None),
        };

        metrics::get_metrics().add_network_bytes_out(packet.bytes.len() as u64, &[]);
        Ok(Some(packet))
    }
}
