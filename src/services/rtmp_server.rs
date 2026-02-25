use super::env_var;

use crate::publish::RtmpServer;

pub struct RtmpServerFactory {
    rtmp_port: Option<u16>,
    rtmp_app: Option<String>,
}

impl RtmpServerFactory {
    pub fn new() -> Self {
        Self {
            rtmp_port: None,
            rtmp_app: None,
        }
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.rtmp_port = Some(port);
        self
    }

    pub fn with_appname(mut self, appname: String) -> Self {
        self.rtmp_app = Some(appname);
        self
    }

    pub fn create(&self) -> RtmpServer {
        RtmpServer::new(
            self.rtmp_port.unwrap_or(1935),
            self.rtmp_app.clone().unwrap_or("lives".to_string()),
        )
    }
}

impl Default for RtmpServerFactory {
    fn default() -> Self {
        Self::new()
            .with_port(env_var("RTMP_PORT").unwrap().parse().unwrap())
            .with_appname(env_var("RTMP_APP").unwrap())
    }
}
