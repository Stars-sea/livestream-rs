use crate::transport::rtmp::SessionGuard;

pub struct PublishHandler {
    session: SessionGuard,

    appname: String,
    stream_key: String,
}

impl PublishHandler {
    pub fn new(session: SessionGuard, appname: String, stream_key: String) -> Self {
        Self {
            session,
            appname,
            stream_key,
        }
    }
}
