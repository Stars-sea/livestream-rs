use std::sync::Arc;

use anyhow::Result;
use rml_rtmp::sessions::{ServerSession, ServerSessionResult};
use rml_rtmp::time::RtmpTimestamp;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::broadcast;

use crate::core::flv_parser::FlvTag;
use crate::egress::dispatcher::{StreamDispatcher, StreamState};
use crate::otlp::metrics;
use crate::server::contracts::StreamRegistry;

pub struct RtmpEgressHandler {
    dispatcher: StreamDispatcher,
    stream_registry: Arc<dyn StreamRegistry>,
    active_stream_rx: Option<broadcast::Receiver<Arc<FlvTag>>>,
    current_stream_id: u32,
    stream_state: Option<StreamState>,
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

            let (rx, state) = self.dispatcher.subscribe(&stream_key).await;
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
            Some(rx) => rx.recv().await.map_err(|e| e.into()),
            None => std::future::pending().await,
        }
    }

    pub async fn handle_broadcast_tag(
        &mut self,
        session: &mut ServerSession,
        socket: &mut TcpStream,
        tag: Arc<FlvTag>,
    ) -> Result<()> {
        if !self.sent_headers {
            self.send_cached_headers(session, socket).await?;
            self.sent_headers = true;
        }

        self.send_tag(session, socket, tag.as_ref()).await
    }

    async fn send_cached_headers(
        &mut self,
        session: &mut ServerSession,
        socket: &mut TcpStream,
    ) -> Result<()> {
        let state = if let Some(s) = &self.stream_state {
            s.clone()
        } else {
            return Ok(());
        };

        if let Some(tag) = state.video_seq_header.read().await.clone() {
            self.send_tag(session, socket, &tag).await?;
        }

        if let Some(tag) = state.audio_seq_header.read().await.clone() {
            self.send_tag(session, socket, &tag).await?;
        }

        Ok(())
    }

    async fn send_tag(
        &mut self,
        session: &mut ServerSession,
        socket: &mut TcpStream,
        tag: &FlvTag,
    ) -> Result<()> {
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
            FlvTag::ScriptData { .. } => return Ok(()),
        };

        socket.write_all(&packet.bytes).await?;
        metrics::get_metrics().add_network_bytes_out(packet.bytes.len() as u64, &[]);
        Ok(())
    }
}
