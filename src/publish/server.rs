use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tracing::{debug, error, info, instrument};

use super::connection::RtmpConnection;
use super::dispatcher::StreamDispatcher;

use crate::core::flv_parser::{FlvDemuxer, FlvTag};
use crate::core::output::FlvPacket;
use crate::ingest::StreamManager;
use crate::settings::PublishConfig;
use crate::settings::load_settings;

#[derive(Debug)]
pub struct RtmpServer {
    config: PublishConfig,

    ingest_manager: Arc<StreamManager>,
}

impl RtmpServer {
    pub fn new(ingest_manager: Arc<StreamManager>) -> Self {
        let config = load_settings().publish.clone();
        Self {
            config,
            ingest_manager,
        }
    }

    #[instrument(name = "publish.rtmp.server.start", skip(self, flv_packet_rx, shutdown), fields(server.port = self.config.port))]
    pub async fn start(
        &self,
        flv_packet_rx: mpsc::UnboundedReceiver<FlvPacket>,
        mut shutdown: broadcast::Receiver<()>,
    ) -> Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", self.config.port)).await?;
        info!(port = self.config.port, "RTMP Server listening");

        let dispatcher = StreamDispatcher::new();

        // Background Task: Receive packets from source, demux, and dispatch
        // We need to pass shutdown signal here too ideally, or just drop rx when main server stops
        let mut shutdown_clone = shutdown.resubscribe();
        let dispatcher_clone = dispatcher.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = process_flv_packets(flv_packet_rx, dispatcher_clone) => {},
                _ = shutdown_clone.recv() => {
                    info!("FLV Packet processor shutting down");
                }
            }
        });

        loop {
            tokio::select! {
                _ = shutdown.recv() => {
                    info!("RTMP Server received shutdown signal");
                    break;
                }
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((socket, addr)) => {
                            debug!(remote_addr = %addr, "New RTMP connection");

                            let mut connection = RtmpConnection::new(
                                socket,
                                dispatcher.clone(),
                                self.config.appname.clone(),
                                self.ingest_manager.clone(),
                            );
                            tokio::spawn(async move {
                                if let Err(e) = connection.run().await {
                                    error!(remote_addr = %addr, error = %e, "RTMP connection error");
                                }

                                debug!(remote_addr = %addr, "RTMP connection closed");
                            });
                        }
                        Err(e) => {
                             error!(error = %e, "Failed to accept RTMP connection");
                             continue;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[instrument(name = "publish.rtmp.flv.process", skip(flv_rx, dispatcher))]
async fn process_flv_packets(
    mut flv_rx: mpsc::UnboundedReceiver<FlvPacket>,
    dispatcher: StreamDispatcher,
) {
    let mut demuxers: HashMap<String, FlvDemuxer> = HashMap::new();

    while let Some(packet) = flv_rx.recv().await {
        let live_id = packet.live_id().to_string();
        let demuxer = demuxers.entry(live_id.clone()).or_insert_with(|| {
            debug!(live_id = %live_id, "Starting FLV demuxing context");
            FlvDemuxer::new()
        });

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
                info!(live_id = %live_id, "Stream ended (FLV Source)");
                demuxers.remove(&live_id);
                dispatcher.remove_stream(&live_id).await;
            }
        }
    }
}
