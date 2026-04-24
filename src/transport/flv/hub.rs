use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::channel::FlvLiveChannel;
use crate::channel::{BroadcastRx, MpscRx, RecvError};
use crate::dispatcher::{self, SessionEvent};
use crate::infra::media::packet::FlvTag;
use crate::transport::abstraction::IngestPacket;
use crate::transport::registry;
use crate::transport::registry::state::SessionState;

pub struct FlvEgressHub {
    channels: DashMap<String, Arc<FlvLiveChannel>>,
}

impl FlvEgressHub {
    pub fn new() -> Self {
        Self {
            channels: DashMap::new(),
        }
    }

    pub async fn subscribe(&self, live_id: &str) -> (BroadcastRx<FlvTag>, Vec<FlvTag>) {
        let channel = self.channel(live_id);
        channel.subscribe().await
    }

    pub fn clear_stream(&self, live_id: &str) {
        self.channels.remove(live_id);
    }

    pub async fn run(
        self: Arc<Self>,
        mut tag_stream: MpscRx<IngestPacket<FlvTag>>,
        cancel_token: CancellationToken,
    ) -> Result<()> {
        let mut events = dispatcher::INSTANCE.subscribe_global();

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => return Ok(()),
                event = events.recv() => self.handle_event(event),
                packet = tag_stream.next() => {
                    let Some(packet) = packet else {
                        return Ok(());
                    };
                    self.distribute(packet).await;
                }
            }
        }
    }

    async fn distribute(&self, packet: IngestPacket<FlvTag>) {
        let live_id = packet.live_id().to_string();
        let Some(state) = registry::INSTANCE.get_state(&live_id).await else {
            self.clear_stream(&live_id);
            debug!(live_id = %live_id, "Dropping FLV tag for unknown session");
            return;
        };

        if state != SessionState::Connected {
            self.clear_stream(&live_id);
            debug!(live_id = %live_id, state = ?state, "Dropping FLV tag for inactive session");
            return;
        }

        let channel = self.channel(&live_id);
        if channel.broadcast_tag(packet.into_packet()).await.is_err() {
            debug!(live_id = %live_id, "No active FLV egress subscribers");
        }
    }

    fn handle_event(&self, event: Result<SessionEvent, RecvError>) {
        match event {
            Ok(SessionEvent::SessionEnded { live_id, .. }) => {
                self.clear_stream(&live_id);
                debug!(live_id = %live_id, "Cleared FLV egress cache after session ended");
            }
            Ok(_) | Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => {}
        }
    }

    fn channel(&self, live_id: &str) -> Arc<FlvLiveChannel> {
        self.channels
            .entry(live_id.to_string())
            .or_insert_with(|| Arc::new(FlvLiveChannel::new(live_id)))
            .clone()
    }
}
