use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use dashmap::DashMap;
use tokio::sync::RwLock;
use tracing::{error, warn};

use super::{Pipe, UnifiedPacketContext, UnifiedPipeFactory};
use crate::abstraction::{PipeContextTrait, PipeTrait};
use crate::dispatcher::{self, SessionEvent};
use crate::pipeline::PipeFactory;

#[derive(Clone)]
pub struct PipeBus {
    packet_pipes: Arc<DashMap<String, Arc<Pipe<UnifiedPacketContext>>>>,
    fallback_pipe: Arc<RwLock<Option<Arc<Pipe<UnifiedPacketContext>>>>>,
    force_fallback: Arc<AtomicBool>,
}

impl PipeBus {
    pub fn new() -> Self {
        Self {
            packet_pipes: Arc::new(DashMap::new()),
            fallback_pipe: Arc::new(RwLock::new(None)),
            force_fallback: Arc::new(AtomicBool::new(false)),
        }
    }

    #[allow(unused)]
    pub async fn set_fallback_pipe(&self, pipe: Arc<Pipe<UnifiedPacketContext>>) {
        let mut guard = self.fallback_pipe.write().await;
        *guard = Some(pipe);
    }

    #[allow(unused)]
    pub async fn clear_fallback_pipe(&self) {
        let mut guard = self.fallback_pipe.write().await;
        *guard = None;
    }

    #[allow(unused)]
    pub fn enable_force_fallback(&self) {
        self.force_fallback.store(true, Ordering::Relaxed);
    }

    #[allow(unused)]
    pub fn disable_force_fallback(&self) {
        self.force_fallback.store(false, Ordering::Relaxed);
    }

    pub fn register_pipe(&self, stream_id: String, pipe: Arc<Pipe<UnifiedPacketContext>>) {
        self.packet_pipes.insert(stream_id, pipe);
    }

    pub fn remove_pipe(&self, stream_id: &str) -> Option<Arc<Pipe<UnifiedPacketContext>>> {
        self.packet_pipes.remove(stream_id).map(|(_, pipe)| pipe)
    }

    pub fn spawn_session_listener(&self, factory: Arc<UnifiedPipeFactory>) {
        let bus = self.clone();

        tokio::spawn(async move {
            let dispatcher = dispatcher::singleton().await;
            let mut rx = dispatcher.subscribe();

            while let Ok(event) = rx.recv().await {
                if let Err(e) = bus.handle_session_event(event, factory.as_ref()) {
                    error!(error = %e, "Failed to handle session event in PipeBus listener");
                }
            }
        });
    }

    fn handle_session_event(
        &self,
        event: SessionEvent,
        factory: &UnifiedPipeFactory,
    ) -> Result<()> {
        match event {
            SessionEvent::SessionInit { live_id, streams } => {
                let pipe = factory.create(live_id.clone(), streams)?;
                self.register_pipe(live_id, Arc::new(pipe));
            }
            SessionEvent::SessionEnded { live_id, .. } => {
                let _ = self.remove_pipe(&live_id);
            }
            _ => {}
        }

        Ok(())
    }

    pub async fn send_packet(&self, context: UnifiedPacketContext) -> Result<UnifiedPacketContext> {
        if self.force_fallback.load(Ordering::Relaxed) {
            let fallback = self.fallback_pipe.read().await.clone();
            if let Some(pipe) = fallback {
                return pipe.send(context).await;
            }

            warn!("Force fallback is enabled but fallback pipe is not configured");
            anyhow::bail!("Fallback mode enabled without fallback pipe");
        }

        let stream_id = context.id();
        let Some(packet_pipe) = self.packet_pipes.get(&stream_id).map(|entry| entry.clone()) else {
            let fallback = self.fallback_pipe.read().await.clone();
            if let Some(pipe) = fallback {
                warn!(stream_id = %stream_id, "No stream pipeline found, routed to fallback pipe");
                return pipe.send(context).await;
            }

            warn!(stream_id = %stream_id, "No pipeline registered for stream, packet dropped");
            anyhow::bail!("No pipeline registered for stream_id: {}", stream_id);
        };

        packet_pipe.send(context).await
    }
}
