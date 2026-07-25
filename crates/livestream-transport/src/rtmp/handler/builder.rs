use std::sync::Arc;

use anyhow::Result;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::{Handler, PlayHandler, PublishHandler};
use crate::lifecycle::HandlerLifecycle;
use crate::rtmp::session::SessionGuard;
use livestream_media::flv::FlvTag;
use livestream_pipeline::broadcast::FlvBroadcast;

pub enum HandlerBuilder {
    Play {
        stream_id: u32,
        session: Option<SessionGuard>,
        stream_key: String,
        tag_stream: Option<broadcast::Receiver<FlvTag>>,
        cached_tags: Vec<FlvTag>,
        cancel_token: Option<CancellationToken>,
    },
    Publish {
        session: Option<SessionGuard>,
        stream_key: String,
        flv_broadcast: Option<Arc<dyn FlvBroadcast>>,
        lifecycle: Option<HandlerLifecycle>,
        cancel_token: Option<CancellationToken>,
    },
}

impl HandlerBuilder {
    pub fn play(stream_key: String, stream_id: u32) -> Self {
        HandlerBuilder::Play {
            session: None,
            stream_key,
            stream_id,
            tag_stream: None,
            cached_tags: Vec::new(),
            cancel_token: None,
        }
    }

    pub fn publish(stream_key: String) -> Self {
        HandlerBuilder::Publish {
            session: None,
            stream_key,
            flv_broadcast: None,
            lifecycle: None,
            cancel_token: None,
        }
    }

    pub fn stream_key(&self) -> &str {
        match self {
            HandlerBuilder::Play { stream_key, .. } => stream_key,
            HandlerBuilder::Publish { stream_key, .. } => stream_key,
        }
    }

    /// App name is validated at the SessionGuard level; this is a no-op
    /// retained for API compatibility.
    pub fn with_appname(self, _appname: String) -> Self {
        self
    }

    pub fn with_session(mut self, session: SessionGuard) -> Self {
        match &mut self {
            HandlerBuilder::Play { session: s, .. } => *s = Some(session),
            HandlerBuilder::Publish { session: s, .. } => *s = Some(session),
        }
        self
    }

    pub fn with_tag_stream(mut self, stream: broadcast::Receiver<FlvTag>) -> Self {
        if let HandlerBuilder::Play { tag_stream, .. } = &mut self {
            *tag_stream = Some(stream);
        }
        self
    }

    pub fn with_cached_tags(mut self, tags: Vec<FlvTag>) -> Self {
        if let HandlerBuilder::Play { cached_tags, .. } = &mut self {
            *cached_tags = tags;
        }
        self
    }

    pub fn with_flv_broadcast(mut self, broadcast: Arc<dyn FlvBroadcast>) -> Self {
        if let HandlerBuilder::Publish { flv_broadcast, .. } = &mut self {
            *flv_broadcast = Some(broadcast);
        }
        self
    }

    pub fn with_lifecycle(mut self, lifecycle: HandlerLifecycle) -> Self {
        if let HandlerBuilder::Publish { lifecycle: l, .. } = &mut self {
            *l = Some(lifecycle);
        }
        self
    }

    pub fn with_cancel_token(mut self, ct: CancellationToken) -> Self {
        match &mut self {
            HandlerBuilder::Play { cancel_token, .. } => *cancel_token = Some(ct),
            HandlerBuilder::Publish { cancel_token, .. } => *cancel_token = Some(ct),
        }
        self
    }

    pub fn build(self) -> Result<Handler> {
        match self {
            HandlerBuilder::Play {
                session,
                stream_key,
                stream_id,
                tag_stream,
                cached_tags,
                cancel_token,
                ..
            } => {
                let session = session
                    .ok_or_else(|| anyhow::anyhow!("Session is required to build PlayHandler"))?;
                let tag_stream = tag_stream.ok_or_else(|| {
                    anyhow::anyhow!("FLV tag receiver is required to build PlayHandler")
                })?;
                let cancel_token = cancel_token.ok_or_else(|| {
                    anyhow::anyhow!("Cancellation token is required to build PlayHandler")
                })?;
                Ok(Handler::Play(PlayHandler::new(
                    session,
                    stream_key,
                    stream_id,
                    tag_stream,
                    cached_tags,
                    cancel_token,
                )))
            }
            HandlerBuilder::Publish {
                session,
                stream_key,
                flv_broadcast,
                lifecycle,
                cancel_token,
                ..
            } => {
                let session = session.ok_or_else(|| {
                    anyhow::anyhow!("Session is required to build PublishHandler")
                })?;
                let flv_broadcast = flv_broadcast.ok_or_else(|| {
                    anyhow::anyhow!("FLV broadcast is required to build PublishHandler")
                })?;
                let cancel_token = cancel_token.ok_or_else(|| {
                    anyhow::anyhow!("Cancellation token is required to build PublishHandler")
                })?;
                let lifecycle = lifecycle.ok_or_else(|| {
                    anyhow::anyhow!("Handler lifecycle is required to build PublishHandler")
                })?;
                Ok(Handler::Publish(PublishHandler::new(
                    session,
                    stream_key,
                    flv_broadcast,
                    lifecycle,
                    cancel_token,
                )))
            }
        }
    }
}
