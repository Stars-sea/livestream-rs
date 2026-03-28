use anyhow::Result;
use crossfire::{MAsyncRx, MAsyncTx, mpmc::List};
use tokio_util::sync::CancellationToken;

use crate::media::format::FlvTag;
use crate::transport::rtmp::handler::Handler;
use crate::transport::rtmp::{PlayHandler, PublishHandler, SessionGuard};

pub enum HandlerBuilder {
    Play {
        stream_id: u32,
        session: Option<SessionGuard>,
        appname: Option<String>,
        stream_key: String,
        flv_tag_rx: Option<MAsyncRx<List<FlvTag>>>,
        cancel_token: Option<CancellationToken>,
    },
    Publish {
        session: Option<SessionGuard>,
        appname: Option<String>,
        stream_key: String,
        flv_tag_tx: Option<MAsyncTx<List<FlvTag>>>,
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
            flv_tag_rx: None,
            cancel_token: None,
        }
    }

    pub fn publish(stream_key: String) -> Self {
        HandlerBuilder::Publish {
            session: None,
            appname: None,
            stream_key,
            flv_tag_tx: None,
            cancel_token: None,
        }
    }

    pub fn is_play(&self) -> bool {
        matches!(self, HandlerBuilder::Play { .. })
    }

    pub fn is_publish(&self) -> bool {
        matches!(self, HandlerBuilder::Publish { .. })
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

    pub fn with_flv_tag_rx(mut self, flv_tag_rx: MAsyncRx<List<FlvTag>>) -> Self {
        if let HandlerBuilder::Play { flv_tag_rx: rx, .. } = &mut self {
            *rx = Some(flv_tag_rx);
        }
        self
    }

    pub fn with_flv_tag_tx(mut self, flv_tag_tx: MAsyncTx<List<FlvTag>>) -> Self {
        if let HandlerBuilder::Publish { flv_tag_tx: tx, .. } = &mut self {
            *tx = Some(flv_tag_tx);
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
                flv_tag_rx,
                cancel_token,
            } => {
                let appname = appname
                    .ok_or_else(|| anyhow::anyhow!("App name is required to build PlayHandler"))?;
                let session = session
                    .ok_or_else(|| anyhow::anyhow!("Session is required to build PlayHandler"))?;
                let flv_tag_rx = flv_tag_rx.ok_or_else(|| {
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
                    flv_tag_rx,
                    cancel_token,
                )))
            }
            HandlerBuilder::Publish {
                session,
                appname,
                stream_key,
                flv_tag_tx,
                cancel_token,
            } => {
                let appname = appname.ok_or_else(|| {
                    anyhow::anyhow!("App name is required to build PublishHandler")
                })?;
                let session = session.ok_or_else(|| {
                    anyhow::anyhow!("Session is required to build PublishHandler")
                })?;
                let flv_tag_tx = flv_tag_tx.ok_or_else(|| {
                    anyhow::anyhow!("FLV tag sender is required to build PublishHandler")
                })?;
                let cancel_token = cancel_token.ok_or_else(|| {
                    anyhow::anyhow!("Cancellation token is required to build PublishHandler")
                })?;
                Ok(Handler::Publish(PublishHandler::new(
                    session,
                    appname,
                    stream_key,
                    flv_tag_tx,
                    cancel_token,
                )))
            }
        }
    }
}
