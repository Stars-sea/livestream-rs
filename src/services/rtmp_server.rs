use tokio::sync::mpsc;

use crate::{core::output::FlvPacket, rtmp_server::RtmpServer, services::env_var};

pub struct RtmpServerFactory {
    rtmp_port: u16,

    flv_packet_tx: Option<mpsc::Sender<FlvPacket>>,
}

impl RtmpServerFactory {
    pub fn new(rtmp_port: u16) -> Self {
        Self {
            rtmp_port,
            flv_packet_tx: None,
        }
    }

    pub fn create(&mut self) -> RtmpServer {
        let (flv_packet_tx, flv_packet_rx) = mpsc::channel(100);
        self.flv_packet_tx = Some(flv_packet_tx);

        RtmpServer::new(self.rtmp_port, flv_packet_rx)
    }

    pub fn get_flv_packet_sender(&self) -> mpsc::Sender<FlvPacket> {
        self.flv_packet_tx.clone().unwrap()
    }
}

impl Default for RtmpServerFactory {
    fn default() -> Self {
        Self::new(env_var("RTMP_PORT").unwrap().parse().unwrap())
    }
}
