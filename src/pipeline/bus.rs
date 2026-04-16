use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use dashmap::{DashMap, mapref::entry::Entry};
use tokio::sync::{Notify, RwLock};
use tokio::time::{Duration, timeout};
use tracing::{error, warn};

use super::{Pipe, UnifiedPacketContext, UnifiedPipeFactory};
use crate::abstraction::{PipeContextTrait, PipeTrait};
use crate::dispatcher::{self, SessionEvent};
use crate::{metric_pipeline_stream_ended, metric_pipeline_stream_started};
use crate::pipeline::PipeFactory;

enum StreamPipeState {
    Pending(Arc<Notify>),
    Ready(Arc<Pipe<UnifiedPacketContext>>),
}

#[derive(Clone)]
pub struct PipeBus {
    stream_states: Arc<DashMap<String, StreamPipeState>>,
    fallback_pipe: Arc<RwLock<Option<Arc<Pipe<UnifiedPacketContext>>>>>,
    force_fallback: Arc<AtomicBool>,
}

impl PipeBus {
    pub fn new() -> Self {
        Self {
            stream_states: Arc::new(DashMap::new()),
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

    pub fn prepare_stream(&self, stream_id: &str) {
        // Pre-allocate readiness signal for this stream.
        // This avoids DashMap shard write-lock contention during the critical
        // high-concurrency window when the first frames of a stream arrive.
        self.stream_states
            .entry(stream_id.to_string())
            .or_insert_with(|| StreamPipeState::Pending(Arc::new(Notify::new())));
    }

    pub fn register_pipe(&self, stream_id: String, pipe: Arc<Pipe<UnifiedPacketContext>>) {
        let mut pending_notify = None;

        match self.stream_states.entry(stream_id) {
            Entry::Occupied(mut entry) => {
                if let StreamPipeState::Pending(notify) = entry.get() {
                    pending_notify = Some(notify.clone());
                }
                entry.insert(StreamPipeState::Ready(pipe));
            }
            Entry::Vacant(entry) => {
                entry.insert(StreamPipeState::Ready(pipe));
            }
        }

        // Notify pending packets after state becomes Ready.
        if let Some(notify) = pending_notify {
            notify.notify_waiters();
        }
    }

    pub fn remove_pipe(&self, stream_id: &str) -> Option<Arc<Pipe<UnifiedPacketContext>>> {
        self.stream_states
            .remove(stream_id)
            .and_then(|(_, state)| match state {
                StreamPipeState::Ready(pipe) => Some(pipe),
                StreamPipeState::Pending(_) => None,
            })
    }

    fn ready_pipe(&self, stream_id: &str) -> Option<Arc<Pipe<UnifiedPacketContext>>> {
        self.stream_states
            .get(stream_id)
            .and_then(|entry| match entry.value() {
                StreamPipeState::Ready(pipe) => Some(pipe.clone()),
                StreamPipeState::Pending(_) => None,
            })
    }

    fn pending_notify(&self, stream_id: &str) -> Option<Arc<Notify>> {
        self.stream_states
            .get(stream_id)
            .and_then(|entry| match entry.value() {
                StreamPipeState::Pending(notify) => Some(notify.clone()),
                StreamPipeState::Ready(_) => None,
            })
    }

    fn remove_stale_pending(&self, stream_id: &str, expected_notify: &Arc<Notify>) {
        let should_remove = self
            .stream_states
            .get(stream_id)
            .map(|entry| match entry.value() {
                StreamPipeState::Pending(current_notify) => {
                    Arc::ptr_eq(current_notify, expected_notify)
                }
                StreamPipeState::Ready(_) => false,
            })
            .unwrap_or(false);

        if should_remove {
            self.stream_states.remove(stream_id);
        }
    }

    async fn resolve_pipe(&self, stream_id: &str) -> Option<Arc<Pipe<UnifiedPacketContext>>> {
        if let Some(pipe) = self.ready_pipe(stream_id) {
            return Some(pipe);
        }

        let Some(notify) = self.pending_notify(stream_id) else {
            // Re-check once to reduce the race window before fallback/drop.
            return self.ready_pipe(stream_id);
        };

        // Re-check once to avoid waiting if register happened just before waiting.
        if let Some(pipe) = self.ready_pipe(stream_id) {
            return Some(pipe);
        }

        // Wait up to 1 second for SessionInit to trigger pipeline creation.
        if timeout(Duration::from_millis(1000), notify.notified())
            .await
            .is_err()
        {
            // Garbage-collect stale pending state only if it is still the same notify.
            self.remove_stale_pending(stream_id, &notify);
        }

        self.ready_pipe(stream_id)
    }

    pub fn spawn_session_listener(&self, factory: Arc<UnifiedPipeFactory>) {
        let bus = self.clone();

        tokio::spawn(async move {
            let mut events = dispatcher::INSTANCE.subscribe_global();

            while let Some(event) = events.next().await {
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
                metric_pipeline_stream_started!();
            }
            SessionEvent::SessionEnded { live_id, .. } => {
                if self.remove_pipe(&live_id).is_some() {
                    metric_pipeline_stream_ended!();
                }
            }
            _ => {}
        }

        Ok(())
    }

    pub async fn send_packet(&self, context: UnifiedPacketContext) -> Result<()> {
        let stream_id = context.id();

        if self.force_fallback.load(Ordering::Relaxed) {
            let fallback = self.fallback_pipe.read().await.clone();
            if let Some(pipe) = fallback {
                pipe.send(context).await?;
                return Ok(());
            }

            warn!("Force fallback is enabled but fallback pipe is not configured");
            anyhow::bail!("Fallback mode enabled without fallback pipe");
        }

        let packet_pipe = self.resolve_pipe(stream_id).await;

        let Some(packet_pipe) = packet_pipe else {
            let fallback = self.fallback_pipe.read().await.clone();
            if let Some(pipe) = fallback {
                warn!(stream_id = %stream_id, "No stream pipeline found, routed to fallback pipe");
                pipe.send(context).await?;
                return Ok(());
            }

            warn!(stream_id = %stream_id, "No pipeline registered for stream, packet dropped");
            anyhow::bail!("No pipeline registered for stream_id: {}", stream_id);
        };

        packet_pipe.send(context).await?;
        Ok(())
    }
}
