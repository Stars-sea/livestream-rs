use anyhow::Result;
use crossfire::{MAsyncTx, mpmc::List};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::{Handler, PlayHandler, PublishHandler};
use crate::infra::media::packet::FlvTag;
use crate::transport::rtmp::session::SessionGuard;
use crate::transport::rtmp::tag::WrappedFlvTag;

pub enum HandlerBuilder {
    Play {
        stream_id: u32,
        session: Option<SessionGuard>,
        appname: Option<String>,
        stream_key: String,
        tag_rx: Option<broadcast::Receiver<FlvTag>>,
        cancel_token: Option<CancellationToken>,
    },
    Publish {
        session: Option<SessionGuard>,
        appname: Option<String>,
        stream_key: String,
        tag_tx: Option<MAsyncTx<List<WrappedFlvTag>>>,
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
            tag_rx: None,
            cancel_token: None,
        }
    }

    pub fn publish(stream_key: String) -> Self {
        HandlerBuilder::Publish {
            session: None,
            appname: None,
            stream_key,
            tag_tx: None,
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

    pub fn with_tag_rx(mut self, tag_rx: broadcast::Receiver<FlvTag>) -> Self {
        if let HandlerBuilder::Play { tag_rx: rx, .. } = &mut self {
            *rx = Some(tag_rx);
        }
        self
    }

    pub fn with_tag_tx(mut self, tag_tx: MAsyncTx<List<WrappedFlvTag>>) -> Self {
        if let HandlerBuilder::Publish { tag_tx: tx, .. } = &mut self {
            *tx = Some(tag_tx);
        }
        self
    }

    pub fn with_cancel_token(mut self, cancel_token: CancellationToken) -> Self {
        match &mut self {
            HandlerBuilder::Play {
                cancel_token: ct, ..
            } => *ct = Some(cancel_token),
            HandlerBuilder::Publish {
                cancel_token: ct, ..
            } => *ct = Some(cancel_token),
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
                tag_rx,
                cancel_token,
            } => {
                let appname = appname
                    .ok_or_else(|| anyhow::anyhow!("App name is required to build PlayHandler"))?;
                let session = session
                    .ok_or_else(|| anyhow::anyhow!("Session is required to build PlayHandler"))?;
                let tag_rx = tag_rx.ok_or_else(|| {
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
                    tag_rx,
                    cancel_token,
                )))
            }
            HandlerBuilder::Publish {
                session,
                appname,
                stream_key,
                tag_tx,
                cancel_token,
            } => {
                let appname = appname.ok_or_else(|| {
                    anyhow::anyhow!("App name is required to build PublishHandler")
                })?;
                let session = session.ok_or_else(|| {
                    anyhow::anyhow!("Session is required to build PublishHandler")
                })?;
                let tag_tx = tag_tx.ok_or_else(|| {
                    anyhow::anyhow!("FLV tag sender is required to build PublishHandler")
                })?;
                let cancel_token = cancel_token.ok_or_else(|| {
                    anyhow::anyhow!("Cancellation token is required to build PublishHandler")
                })?;
                Ok(Handler::Publish(PublishHandler::new(
                    session,
                    appname,
                    stream_key,
                    tag_tx,
                    cancel_token,
                )))
            }
        }
    }
}
