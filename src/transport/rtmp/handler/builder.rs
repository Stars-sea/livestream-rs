use anyhow::Result;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::{Handler, PlayHandler, PublishHandler};
use crate::infra::media::packet::FlvTag;
use crate::pipeline::PipeBus;
use crate::queue::ChannelStream;
use crate::transport::lifecycle::HandlerLifecycle;
use crate::transport::rtmp::session::SessionGuard;

pub enum HandlerBuilder {
    Play {
        stream_id: u32,
        session: Option<SessionGuard>,
        appname: Option<String>,
        stream_key: String,
        tag_stream: Option<ChannelStream<broadcast::Receiver<FlvTag>>>,
        cached_tags: Vec<FlvTag>,
        cancel_token: Option<CancellationToken>,
    },
    Publish {
        session: Option<SessionGuard>,
        appname: Option<String>,
        stream_key: String,
        bus: Option<PipeBus>,
        lifecycle: Option<HandlerLifecycle>,
        cancel_token: Option<CancellationToken>,
    },
}

impl HandlerBuilder {
    pub fn play(stream_key: String, stream_id: u32) -> Self {
        HandlerBuilder::Play {
            session: None,
            appname: None,
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
            appname: None,
            stream_key,
            bus: None,
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

    pub fn with_appname(mut self, appname: String) -> Self {
        match &mut self {
            HandlerBuilder::Play { appname: a, .. } => *a = Some(appname),
            HandlerBuilder::Publish { appname: a, .. } => *a = Some(appname),
        }
        self
    }

    pub fn with_session(mut self, session: SessionGuard) -> Self {
        match &mut self {
            HandlerBuilder::Play { session: s, .. } => *s = Some(session),
            HandlerBuilder::Publish { session: s, .. } => *s = Some(session),
        }
        self
    }

    pub fn with_tag_stream(mut self, stream: ChannelStream<broadcast::Receiver<FlvTag>>) -> Self {
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

    pub fn with_pipe_bus(mut self, bus: PipeBus) -> Self {
        if let HandlerBuilder::Publish { bus: b, .. } = &mut self {
            *b = Some(bus);
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
                appname,
                stream_key,
                stream_id,
                tag_stream,
                cached_tags,
                cancel_token,
            } => {
                let appname = appname
                    .ok_or_else(|| anyhow::anyhow!("App name is required to build PlayHandler"))?;
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
                    appname,
                    stream_key,
                    stream_id,
                    tag_stream,
                    cached_tags,
                    cancel_token,
                )))
            }
            HandlerBuilder::Publish {
                session,
                appname,
                stream_key,
                bus,
                lifecycle,
                cancel_token,
            } => {
                let appname = appname.ok_or_else(|| {
                    anyhow::anyhow!("App name is required to build PublishHandler")
                })?;
                let session = session.ok_or_else(|| {
                    anyhow::anyhow!("Session is required to build PublishHandler")
                })?;
                let bus = bus.ok_or_else(|| {
                    anyhow::anyhow!("Pipe bus is required to build PublishHandler")
                })?;
                let cancel_token = cancel_token.ok_or_else(|| {
                    anyhow::anyhow!("Cancellation token is required to build PublishHandler")
                })?;
                let lifecycle = lifecycle.ok_or_else(|| {
                    anyhow::anyhow!("Handler lifecycle is required to build PublishHandler")
                })?;
                Ok(Handler::Publish(PublishHandler::new(
                    session,
                    appname,
                    stream_key,
                    bus,
                    lifecycle,
                    cancel_token,
                )))
            }
        }
    }
}
