use crate::transport::rtmp::SessionGuard;

pub struct PlayHandler {
    session: SessionGuard,

    appname: String,
    stream_key: String,
}

impl PlayHandler {
    pub fn new(session: SessionGuard, appname: String, stream_key: String) -> Self {
        Self {
            session,
            appname,
            stream_key,
        }
    }
}
