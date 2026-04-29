use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use async_stream::stream;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;
use rml_rtmp::sessions::StreamMetadata;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::channel::{BroadcastRx, RecvError};
use crate::config::HttpFlvConfig;
use crate::infra::media::packet::{FlvTag, encode_flv_header, encode_flv_tag};
use crate::transport::flv::FlvEgressHub;
use crate::transport::registry;
use crate::transport::registry::state::SessionState;

const PATH_PREFIX: &str = "/lives";
const ROUTE_PATH: &str = "/lives/{*path}";

pub fn playback_path(live_id: &str) -> String {
    format!("{PATH_PREFIX}/{live_id}.flv")
}

#[derive(Clone)]
struct HttpFlvState {
    flv_egress_hub: Arc<FlvEgressHub>,
    cancel_token: CancellationToken,
}

pub struct HttpFlvServer {
    listener: TcpListener,
    router: Router,
    cancel_token: CancellationToken,
}

impl HttpFlvServer {
    pub async fn create(
        config: HttpFlvConfig,
        flv_egress_hub: Arc<FlvEgressHub>,
        cancel_token: CancellationToken,
    ) -> Result<Self> {
        let route_path = ROUTE_PATH;
        let addr: SocketAddr = format!("0.0.0.0:{}", config.port).parse()?;
        let listener = TcpListener::bind(addr).await?;

        let state = HttpFlvState {
            flv_egress_hub,
            cancel_token: cancel_token.clone(),
        };
        let router = Router::new()
            .route(route_path, get(handle_http_flv).options(handle_cors_preflight))
            .with_state(state);

        info!(address = %addr, route = %route_path, "HTTP-FLV server will listen");

        Ok(Self {
            listener,
            router,
            cancel_token,
        })
    }

    pub async fn run(self) -> Result<()> {
        let cancel_token = self.cancel_token.clone();
        axum::serve(self.listener, self.router)
            .with_graceful_shutdown(async move {
                cancel_token.cancelled().await;
            })
            .await?;
        Ok(())
    }
}

async fn handle_http_flv(State(state): State<HttpFlvState>, Path(path): Path<String>) -> Response {
    let Some(live_id) = parse_live_id(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    match registry::INSTANCE.get_state(live_id).await {
        Some(SessionState::Connected) => stream_response(state, live_id.to_owned()).await,
        Some(SessionState::Pending | SessionState::Connecting | SessionState::Disconnected) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn parse_live_id(path: &str) -> Option<&str> {
    let live_id = path.strip_suffix(".flv")?;

    if live_id.is_empty() || live_id.contains('/') {
        return None;
    }

    Some(live_id)
}

async fn stream_response(state: HttpFlvState, live_id: String) -> Response {
    let (mut tag_stream, cached_tags) = state.flv_egress_hub.subscribe(&live_id).await;
    let header_bytes = build_flv_header(&cached_tags);
    let cancel_token = state.cancel_token.clone();

    let body = Body::from_stream(stream! {
        yield Ok::<Bytes, Infallible>(header_bytes);

        if let Some(encoded_cached_tags) = encode_cached_tags(&live_id, cached_tags) {
            for bytes in encoded_cached_tags {
                yield Ok(bytes);
            }
        } else {
            return;
        }

        let mut waiting_keyframe = false;

        while let Some(bytes) = next_live_chunk(
            &live_id,
            &cancel_token,
            &mut tag_stream,
            &mut waiting_keyframe,
        ).await {
            yield Ok(bytes);
        }
    });

    flv_response(body)
}

fn encode_cached_tags(live_id: &str, cached_tags: Vec<FlvTag>) -> Option<Vec<Bytes>> {
    cached_tags
        .into_iter()
        .map(|tag| encode_tag(live_id, tag, "cached"))
        .collect()
}

async fn next_live_chunk(
    live_id: &str,
    cancel_token: &CancellationToken,
    tag_stream: &mut BroadcastRx<FlvTag>,
    waiting_keyframe: &mut bool,
) -> Option<Bytes> {
    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => return None,
            recv = tag_stream.recv() => match recv {
                Ok(tag) => {
                    if should_skip_while_waiting_keyframe(waiting_keyframe, &tag) {
                        continue;
                    }

                    return encode_tag(live_id, tag, "live");
                }
                Err(RecvError::Lagged(skipped)) => {
                    *waiting_keyframe = true;
                    debug!(live_id = %live_id, skipped = skipped, "HTTP-FLV subscriber lagged, dropping stale tags");
                }
                Err(RecvError::Closed) => return None,
            }
        }
    }
}

fn encode_tag(live_id: &str, tag: FlvTag, phase: &'static str) -> Option<Bytes> {
    match encode_flv_tag(&tag) {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            warn!(error = %error, live_id = %live_id, phase, "Failed to encode FLV tag");
            None
        }
    }
}

fn flv_response(body: Body) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("video/x-flv"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response
}

async fn handle_cors_preflight() -> Response {
    let mut response = Response::new(Body::empty());
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
    response
}

fn should_skip_while_waiting_keyframe(waiting_keyframe: &mut bool, tag: &FlvTag) -> bool {
    if !*waiting_keyframe {
        return false;
    }

    match tag {
        FlvTag::Video {
            is_keyframe: true, ..
        } => {
            *waiting_keyframe = false;
            false
        }
        FlvTag::Video { .. } => true,
        _ => false,
    }
}

fn build_flv_header(cached_tags: &[FlvTag]) -> Bytes {
    let mut has_audio = false;
    let mut has_video = false;

    for tag in cached_tags {
        match tag {
            FlvTag::Audio { .. } => has_audio = true,
            FlvTag::Video { .. } => has_video = true,
            FlvTag::ScriptData(metadata) => {
                apply_metadata_tracks(metadata, &mut has_audio, &mut has_video);
            }
        }
    }

    encode_flv_header(has_audio, has_video)
}

fn apply_metadata_tracks(metadata: &StreamMetadata, has_audio: &mut bool, has_video: &mut bool) {
    *has_audio |= metadata.audio_codec_id.is_some();
    *has_video |= metadata.video_codec_id.is_some();
}

#[cfg(test)]
mod tests {
    use super::{
        ROUTE_PATH, apply_metadata_tracks, build_flv_header, parse_live_id, playback_path,
        should_skip_while_waiting_keyframe,
    };
    use bytes::Bytes;
    use rml_rtmp::sessions::StreamMetadata;

    use crate::infra::media::packet::FlvTag;

    #[test]
    fn builds_expected_route_path() {
        assert_eq!(ROUTE_PATH, "/lives/{*path}");
        assert_eq!(playback_path("demo"), "/lives/demo.flv");
    }

    #[test]
    fn parses_only_flat_flv_live_ids() {
        assert_eq!(parse_live_id("demo.flv"), Some("demo"));
        assert_eq!(parse_live_id("demo"), None);
        assert_eq!(parse_live_id(""), None);
        assert_eq!(parse_live_id("nested/demo.flv"), None);
        assert_eq!(parse_live_id(".flv"), None);
    }

    #[test]
    fn skip_video_until_keyframe_after_lag() {
        let mut waiting_keyframe = true;
        let non_keyframe = FlvTag::video(0, Bytes::from_static(&[0x27, 0x01]));
        let keyframe = FlvTag::video(0, Bytes::from_static(&[0x17, 0x01]));

        assert!(should_skip_while_waiting_keyframe(
            &mut waiting_keyframe,
            &non_keyframe
        ));
        assert!(!should_skip_while_waiting_keyframe(
            &mut waiting_keyframe,
            &keyframe
        ));
        assert!(!waiting_keyframe);
    }

    #[test]
    fn metadata_can_advertise_audio_and_video_tracks() {
        let mut metadata = StreamMetadata::new();
        let mut has_audio = false;
        let mut has_video = false;
        metadata.audio_codec_id = Some(10);
        metadata.video_codec_id = Some(7);

        apply_metadata_tracks(&metadata, &mut has_audio, &mut has_video);

        assert!(has_audio);
        assert!(has_video);
    }

    #[test]
    fn flv_header_uses_script_data_when_sequence_headers_are_missing() {
        let mut metadata = StreamMetadata::new();
        metadata.audio_codec_id = Some(10);
        metadata.video_codec_id = Some(7);

        let header = build_flv_header(&[FlvTag::script_data(metadata)]);

        assert_eq!(header[4] & 0b0000_0100, 0b0000_0100);
        assert_eq!(header[4] & 0b0000_0001, 0b0000_0001);
    }
}
