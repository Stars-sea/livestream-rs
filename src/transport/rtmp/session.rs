use anyhow::Result;
use bytes::BytesMut;
use crossfire::MTx;
use crossfire::mpsc::Array;
use regex::Regex;
use rml_rtmp::chunk_io::{ChunkSerializer, Packet};
use rml_rtmp::messages::RtmpMessage;
use rml_rtmp::rml_amf0::Amf0Value;
use rml_rtmp::sessions::{ServerSession, ServerSessionEvent, ServerSessionResult};
use rml_rtmp::time::RtmpTimestamp;
use std::sync::OnceLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::infra::media::packet::FlvTag;
use crate::transport::contract::message::StreamEvent;
use crate::transport::contract::state::{ConnectionStateTrait, RtmpState, SessionState};
use crate::transport::registry::global;

use super::connection::RtmpConnection;
use super::handler::HandlerBuilder;

pub struct SessionGuard {
    connection: RtmpConnection,
    session: ServerSession,
    appname: String,

    pub(super) event_tx: MTx<Array<StreamEvent>>,

    chunk_size: u32,
}

pub(super) struct SessionGuardBuilder {
    connection: RtmpConnection,
    session: Option<ServerSession>,
    appname: Option<String>,
    event_tx: Option<MTx<Array<StreamEvent>>>,
}

impl SessionGuard {
    pub(self) fn new(
        connection: RtmpConnection,
        session: ServerSession,
        appname: String,
        event_tx: MTx<Array<StreamEvent>>,
    ) -> Self {
        Self {
            connection,
            session,
            appname,
            event_tx,
            chunk_size: 2048,
        }
    }

    pub(super) fn set_chunk_size(&mut self, new_chunk_size: u32) {
        self.chunk_size = new_chunk_size;
    }

    pub(super) async fn read_result(
        &mut self,
        ct: &CancellationToken,
    ) -> Result<Vec<ServerSessionResult>> {
        let mut buffer = BytesMut::with_capacity(self.chunk_size as usize);

        let length = self.connection.read(&mut buffer, ct).await?;
        if length == 0 {
            anyhow::bail!("Connection closed");
        }

        Ok(self.session.handle_input(&buffer)?)
    }

    async fn handle_packet(&mut self, packet: Packet, ct: &CancellationToken) -> Result<()> {
        self.connection.write(&packet.bytes, ct).await?;
        Ok(())
    }

    async fn send_optional_command_result(
        &mut self,
        transaction_id: f64,
        additional_arguments: Vec<Amf0Value>,
        ct: &CancellationToken,
    ) -> Result<()> {
        let message = RtmpMessage::Amf0Command {
            command_name: "_result".to_string(),
            transaction_id,
            command_object: Amf0Value::Null,
            additional_arguments,
        };

        // Use a fresh serializer with full headers to avoid state coupling with ServerSession internals.
        let payload = message.into_message_payload(RtmpTimestamp::new(0), 0)?;
        let mut serializer = ChunkSerializer::new();
        let packet = serializer.serialize(&payload, true, false)?;
        self.handle_packet(packet, ct).await
    }

    async fn handle_optional_amf0_command(
        &mut self,
        command_name: &str,
        transaction_id: f64,
        ct: &CancellationToken,
    ) -> Result<bool> {
        match command_name {
            "_checkbw" => {
                self.send_optional_command_result(transaction_id, Vec::new(), ct)
                    .await?;
                debug!(transaction_id = %transaction_id, "Handled optional AMF0 command _checkbw");
                Ok(true)
            }
            "getStreamLength" => {
                // For live streams, duration is unknown; return 0 as a safe default.
                self.send_optional_command_result(transaction_id, vec![Amf0Value::Number(0.0)], ct)
                    .await?;
                debug!(transaction_id = %transaction_id, "Handled optional AMF0 command getStreamLength");
                Ok(true)
            }
            "releaseStream" => {
                self.send_optional_command_result(transaction_id, Vec::new(), ct)
                    .await?;
                debug!(transaction_id = %transaction_id, "Handled optional AMF0 command releaseStream");
                Ok(true)
            }
            "FCPublish" => {
                self.send_optional_command_result(transaction_id, Vec::new(), ct)
                    .await?;
                debug!(transaction_id = %transaction_id, "Handled optional AMF0 command FCPublish");
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    pub(super) async fn handle_results(
        &mut self,
        results: Vec<ServerSessionResult>,
        ct: &CancellationToken,
    ) -> Result<Vec<ServerSessionEvent>> {
        let mut events = Vec::new();
        for result in results {
            match result {
                ServerSessionResult::OutboundResponse(packet) => {
                    self.handle_packet(packet, ct).await?;
                }
                ServerSessionResult::RaisedEvent(event) => {
                    events.push(event);
                }
                ServerSessionResult::UnhandleableMessageReceived(payload) => {
                    // TODO: Handle unhandleable messages properly
                    debug!(payload = ?payload, "Received non-response RTMP session result, ignoring.");
                }
            }
        }

        Ok(events)
    }

    pub(super) async fn send_flv_tag(
        &mut self,
        stream_id: u32,
        tag: FlvTag,
        ct: &CancellationToken,
    ) -> Result<()> {
        fn to_timestamp(timestamp: u32) -> RtmpTimestamp {
            RtmpTimestamp::new(timestamp)
        }

        let packet = match tag {
            FlvTag::Audio { timestamp, payload } => {
                self.session
                    .send_audio_data(stream_id, payload, to_timestamp(timestamp), false)?
            }
            FlvTag::Video {
                timestamp,
                payload,
                is_keyframe,
            } => self.session.send_video_data(
                stream_id,
                payload,
                to_timestamp(timestamp),
                is_keyframe,
            )?,
            FlvTag::ScriptData(metadata) => self.session.send_metadata(stream_id, &metadata)?,
        };

        self.handle_packet(packet, ct).await?;

        Ok(())
    }

    pub(super) async fn finish_playing(
        &mut self,
        stream_id: u32,
        ct: &CancellationToken,
    ) -> Result<()> {
        let packet = self.session.finish_playing(stream_id)?;
        self.handle_packet(packet, ct).await?;
        Ok(())
    }

    pub async fn connect(mut self, ct: &CancellationToken) -> Result<HandlerBuilder> {
        loop {
            let results = self.read_result(ct).await?;

            let events = self.handle_results(results, ct).await?;
            for event in events {
                let handler_builder = match self.handle_connect_event(event, ct).await? {
                    Some(builder) => builder,
                    None => continue,
                };

                let handler_builder = handler_builder
                    .with_appname(self.appname.clone())
                    .with_session(self);
                return Ok(handler_builder);
            }
        }
    }

    async fn accept_request(&mut self, request_id: u32, ct: &CancellationToken) -> Result<()> {
        let results = self.session.accept_request(request_id)?;
        self.handle_results(results, ct).await?;
        Ok(())
    }

    async fn reject_request(
        &mut self,
        request_id: u32,
        code: &str,
        description: &str,
        ct: &CancellationToken,
    ) -> Result<()> {
        let results = self.session.reject_request(request_id, code, description)?;
        self.handle_results(results, ct).await?;
        Ok(())
    }

    async fn handle_connect_event(
        &mut self,
        event: ServerSessionEvent,
        ct: &CancellationToken,
    ) -> Result<Option<HandlerBuilder>> {
        static APP_AND_STREAM_RE: OnceLock<Regex> = OnceLock::new();
        let app_and_stream_re = APP_AND_STREAM_RE.get_or_init(|| {
            Regex::new(r"^/?(?P<app>[^/?]+)(?:/(?P<stream_key>[^?]+))?(?:\?.*)?$")
                .expect("valid app/stream regex")
        });

        match event {
            ServerSessionEvent::ConnectionRequested {
                request_id,
                app_name,
            } => {
                if let Some(captures) = app_and_stream_re.captures(&app_name) {
                    let incoming_app = captures.name("app").map(|m| m.as_str()).unwrap_or_default();

                    if incoming_app == self.appname.as_str() {
                        self.on_connect_requested(request_id, self.appname.clone(), ct)
                            .await?;
                        return Ok(None);
                    }
                }

                self.on_connect_requested(request_id, app_name, ct).await?;
                Ok(None)
            }
            ServerSessionEvent::PlayStreamRequested {
                request_id,
                stream_key,
                stream_id,
                ..
            } => {
                let stream_key = app_and_stream_re
                    .captures(&stream_key)
                    .and_then(|caps| {
                        caps.name("stream_key")
                            .or_else(|| caps.name("app"))
                            .map(|m| m.as_str().to_string())
                    })
                    .unwrap_or(stream_key)
                    .trim_start_matches('/')
                    .to_string();

                self.on_play_requested(request_id, stream_key, stream_id, ct)
                    .await
            }
            ServerSessionEvent::PublishStreamRequested {
                request_id,
                stream_key,
                app_name,
                ..
            } => {
                info!("Received publish request for stream key: {}", stream_key);
                let stream_key = app_and_stream_re.captures(&stream_key).and_then(|caps| {
                    caps.name("stream_key")
                        .or_else(|| caps.name("app"))
                        .map(|m| m.as_str().to_string())
                });
                let stream_key = stream_key.or_else(|| {
                    app_and_stream_re.captures(&app_name).and_then(|caps| {
                        caps.name("stream_key")
                            .map(|m| m.as_str().to_string())
                            .filter(|s| !s.is_empty())
                    })
                });
                let stream_key = stream_key.unwrap_or_default();

                self.on_publish_requested(request_id, stream_key, ct).await
            }
            ServerSessionEvent::UnhandleableAmf0Command {
                command_name,
                transaction_id,
                ..
            } => {
                if self
                    .handle_optional_amf0_command(&command_name, transaction_id, ct)
                    .await?
                {
                    return Ok(None);
                }

                warn!(command_name = %command_name, transaction_id = %transaction_id, "Unhandled AMF0 command");
                Ok(None)
            }

            _ => {
                debug!(event = ?event, "Unhandled session event");
                anyhow::bail!("Unhandled session event: {:?}", event);
            }
        }
    }

    async fn on_connect_requested(
        &mut self,
        request_id: u32,
        app_name: String,
        ct: &CancellationToken,
    ) -> Result<()> {
        if app_name != self.appname {
            debug!(app_name = %app_name, expected_app = %self.appname, "Client requested connection to unexpected app");
            self.reject_request(request_id, "AppNotFound", "Application not found", ct)
                .await?;
            anyhow::bail!(
                "Client requested connection to unexpected app: {}",
                app_name
            );
        }

        self.accept_request(request_id, ct).await?;
        Ok(())
    }

    async fn on_play_requested(
        &mut self,
        request_id: u32,
        stream_key: String,
        stream_id: u32,
        ct: &CancellationToken,
    ) -> Result<Option<HandlerBuilder>> {
        let is_active = match global::get_session_state(&stream_key).await {
            Some(state) => state.is_active(),
            None => false,
        };
        if !is_active {
            debug!(stream_key = %stream_key, "Client requested to play non-existent or inactive stream");
            self.reject_request(request_id, "StreamNotFound", "Stream not found", ct)
                .await?;
            anyhow::bail!(
                "Client requested to play non-existent or inactive stream: {}",
                stream_key
            );
        }

        self.accept_request(request_id, ct).await?;
        Ok(Some(HandlerBuilder::play(stream_key, stream_id)))
    }

    async fn on_publish_requested(
        &mut self,
        request_id: u32,
        stream_key: String,
        ct: &CancellationToken,
    ) -> Result<Option<HandlerBuilder>> {
        let state = global::get_session_state(&stream_key).await;
        if state.is_none() {
            debug!(stream_key = %stream_key, "Client requested to publish to a stream that does not exist");
            self.event_tx.send(StreamEvent::StateChange {
                live_id: stream_key.clone(),
                new_state: SessionState::Rtmp(RtmpState::Disconnected),
            })?;
            self.reject_request(request_id, "StreamNotFound", "Stream not found", ct)
                .await?;
            anyhow::bail!(
                "Client requested to publish to a stream that does not exist: {}",
                stream_key
            );
        }

        if matches!(state, Some(SessionState::Rtmp(RtmpState::Pending))) {
            self.event_tx.send(StreamEvent::StateChange {
                live_id: stream_key.clone(),
                new_state: SessionState::Rtmp(RtmpState::Connecting),
            })?;

            let res = self.accept_request(request_id, ct).await;

            let new_state = if res.is_ok() {
                SessionState::Rtmp(RtmpState::Connected)
            } else {
                SessionState::Rtmp(RtmpState::Disconnected)
            };
            self.event_tx.send(StreamEvent::StateChange {
                live_id: stream_key.clone(),
                new_state: new_state,
            })?;

            res?;
        } else {
            debug!(stream_key = %stream_key, "Client requested to publish to a stream that is already active");
            self.event_tx.send(StreamEvent::StateChange {
                live_id: stream_key.clone(),
                new_state: SessionState::Rtmp(RtmpState::Disconnected),
            })?;

            self.reject_request(
                request_id,
                "StreamAlreadyActive",
                "Stream is already active",
                ct,
            )
            .await?;
            anyhow::bail!(
                "Client requested to publish to a stream that is already active: {}",
                stream_key
            );
        }

        Ok(Some(HandlerBuilder::publish(stream_key)))
    }
}

impl SessionGuardBuilder {
    pub fn new(connection: RtmpConnection) -> Self {
        Self {
            connection,
            session: None,
            appname: None,
            event_tx: None,
        }
    }

    pub fn with_session(mut self, session: ServerSession) -> Self {
        self.session = Some(session);
        self
    }

    pub fn with_appname(mut self, appname: String) -> Self {
        self.appname = Some(appname);
        self
    }

    pub fn with_event_tx(mut self, event_tx: MTx<Array<StreamEvent>>) -> Self {
        self.event_tx = Some(event_tx);
        self
    }

    pub fn build(self) -> Result<SessionGuard> {
        let session = self
            .session
            .ok_or_else(|| anyhow::anyhow!("Session is required to build SessionGuard"))?;
        let appname = self
            .appname
            .ok_or_else(|| anyhow::anyhow!("App name is required to build SessionGuard"))?;
        let event_tx = self.event_tx.ok_or_else(|| {
            anyhow::anyhow!("Event transmitter is required to build SessionGuard")
        })?;

        Ok(SessionGuard::new(
            self.connection,
            session,
            appname,
            event_tx,
        ))
    }
}
