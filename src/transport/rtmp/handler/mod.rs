pub mod play;
pub mod publish;

use crate::transport::rtmp::SessionGuard;

use super::{PlayHandler, PublishHandler};

pub enum Handler {
    Play(PlayHandler),
    Publish(PublishHandler),
}

pub enum HandlerType {
    Play,
    Publish,
}

impl Handler {
    pub fn new(
        session: SessionGuard,
        appname: String,
        stream_key: String,
        handler_type: HandlerType,
    ) -> Self {
        match handler_type {
            HandlerType::Play => Self::play(session, appname, stream_key),
            HandlerType::Publish => Self::publish(session, appname, stream_key),
        }
    }

    pub fn play(session: SessionGuard, appname: String, stream_key: String) -> Self {
        Handler::Play(PlayHandler::new(session, appname, stream_key))
    }

    pub fn publish(session: SessionGuard, appname: String, stream_key: String) -> Self {
        Handler::Publish(PublishHandler::new(session, appname, stream_key))
    }
}
