use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use log::{error, info};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use super::connection::RtmpConnection;
use super::dispatcher::StreamDispatcher;

use crate::core::flv_parser::{FlvDemuxer, FlvTag};
use crate::core::output::FlvPacket;
use crate::ingest::StreamManager;

#[derive(Debug)]
pub struct RtmpServer {
    port: u16,
    appname: String,

    ingest_manager: Arc<StreamManager>,
}

impl RtmpServer {
    pub fn new(port: u16, appname: String, ingest_manager: Arc<StreamManager>) -> Self {
        Self {
            port,
            appname,
            ingest_manager,
        }
    }

    pub async fn start(&self, flv_packet_rx: mpsc::UnboundedReceiver<FlvPacket>) -> Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", self.port)).await?;
        info!("RTMP Server listening on 0.0.0.0:{}", self.port);

        let dispatcher = StreamDispatcher::new();

        // Background Task: Receive packets from source, demux, and dispatch
        tokio::spawn(process_flv_packets(flv_packet_rx, dispatcher.clone()));

        loop {
            let (socket, addr) = listener.accept().await?;
            info!("New RTMP connection from {}", addr);

            let mut connection = RtmpConnection::new(
                socket,
                dispatcher.clone(),
                self.appname.clone(),
                self.ingest_manager.clone(),
            );
            tokio::spawn(async move {
                if let Err(e) = connection.run().await {
                    error!("RTMP connection error {}: {}", addr, e);
                }

                info!("RTMP connection closed: {}", addr);
            });
        }
    }
}

async fn process_flv_packets(
    mut flv_rx: mpsc::UnboundedReceiver<FlvPacket>,
    dispatcher: StreamDispatcher,
) {
    let mut demuxers: HashMap<String, FlvDemuxer> = HashMap::new();

    while let Some(packet) = flv_rx.recv().await {
        let live_id = packet.live_id().to_string();
        let demuxer = demuxers
            .entry(live_id.clone())
            .or_insert_with(FlvDemuxer::new);

        match packet {
            FlvPacket::Data { data, .. } => {
                demuxer.push_data(&data);
                while let Some(tag) = demuxer.next_tag() {
                    let tag = Arc::new(tag);

                    // Cache sequence headers
                    let state = dispatcher.stream(&live_id).await;

                    match tag.as_ref() {
                        FlvTag::Video { payload, .. } => {
                            if payload.len() > 1 && payload[1] == 0 {
                                // AVCPacketType == 0 (Sequence Header)
                                *state.video_seq_header.write().await = Some(tag.clone());
                            }
                        }
                        FlvTag::Audio { payload, .. } => {
                            if payload.len() > 1 && payload[1] == 0 {
                                // AACPacketType == 0 (Sequence Header)
                                *state.audio_seq_header.write().await = Some(tag.clone());
                            }
                        }
                        FlvTag::ScriptData { .. } => {
                            *state.metadata.write().await = Some(tag.clone());
                        }
                    }

                    // Ignore error if no subscribers
                    let _ = state.sender.send(tag);
                }
            }
            FlvPacket::EndOfStream { live_id } => {
                info!("Stream ended: {}", live_id);
                demuxers.remove(&live_id);
                dispatcher.remove_stream(&live_id).await;
            }
        }
    }
}
