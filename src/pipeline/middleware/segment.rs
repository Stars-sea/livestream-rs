use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tempfile::{Builder, TempDir};
use tracing::warn;

use crate::abstraction::{MiddlewareTrait, PipeContextTrait};
use crate::dispatcher::{self, SessionEvent};
use crate::infra::ContextSlotMap;
use crate::infra::media::StreamCollection;
use crate::infra::media::context::HlsOutputContext;
use crate::infra::media::packet::{Packet, UnifiedPacket};
use crate::pipeline::UnifiedPacketContext;
use crate::pipeline::normalize::normalize_component;

struct SegmentState {
    hls_ctx: Option<HlsOutputContext>,
    temp_dir: TempDir,
    streams: Arc<dyn StreamCollection + Send + Sync>,
    segment_id: u64,
    segment_started_at: Instant,
}

impl SegmentState {
    fn new(
        temp_dir: TempDir,
        hls_ctx: HlsOutputContext,
        streams: Arc<dyn StreamCollection + Send + Sync>,
    ) -> Self {
        Self {
            hls_ctx: Some(hls_ctx),
            temp_dir,
            streams,
            segment_id: 0,
            segment_started_at: Instant::now(),
        }
    }

    fn should_rollover(&self, packet: &Packet, segment_duration: Duration) -> bool {
        packet.is_key_frame() && self.segment_started_at.elapsed() >= segment_duration
    }

    fn rollover(&mut self) -> Result<PathBuf> {
        let completed_segment_path = self
            .hls_ctx
            .as_ref()
            .map(|ctx| ctx.path().clone())
            .ok_or_else(|| anyhow::anyhow!("Missing active HLS context during rollover"))?;

        self.segment_id = self.segment_id.saturating_add(1);
        let next_ctx = HlsOutputContext::create_segment(
            self.temp_dir.path(),
            self.streams.as_ref(),
            self.segment_id,
        )?;
        self.hls_ctx = Some(next_ctx);
        self.segment_started_at = Instant::now();
        Ok(completed_segment_path)
    }
}

impl Drop for SegmentState {
    fn drop(&mut self) {
        // Drop output context first so file handles are closed before TempDir cleanup.
        self.hls_ctx.take();

        // Touch temp_dir to make intent explicit and silence unused-field warnings.
        let _ = self.temp_dir.path();
    }
}

pub struct SegmentMiddleware {
    contexts: ContextSlotMap<SegmentState>,
    segment_duration: Duration,
}

impl SegmentMiddleware {
    pub fn new(segment_duration: Duration) -> Self {
        let contexts = ContextSlotMap::new();
        Self::spawn_event_listener(contexts.clone());

        Self {
            contexts,
            segment_duration,
        }
    }

    async fn emit_segment_complete(live_id: String, segment_path: PathBuf) {
        dispatcher::singleton()
            .await
            .send(SessionEvent::SegmentComplete {
                live_id,
                path: segment_path,
            });
    }

    fn spawn_event_listener(contexts: ContextSlotMap<SegmentState>) {
        tokio::spawn(async move {
            let dispatcher = dispatcher::singleton().await;
            let mut rx = dispatcher.subscribe();

            while let Ok(event) = rx.recv().await {
                if let Err(e) = Self::session_event_handler(&contexts, event).await {
                    warn!(error = %e, "Error handling session event in SegmentMiddleware");
                }
            }
        });
    }

    async fn session_event_handler(
        contexts: &ContextSlotMap<SegmentState>,
        event: SessionEvent,
    ) -> Result<()> {
        match event {
            SessionEvent::SessionInit { live_id, streams } => {
                let temp_dir = Self::create_stream_temp_dir(&live_id)?;
                let hls_ctx =
                    HlsOutputContext::create_segment(temp_dir.path(), streams.as_ref(), 0)?;

                contexts
                    .with_or_create(&live_id, |state| {
                        *state = Some(SegmentState::new(temp_dir, hls_ctx, streams));
                    })
                    .await;
            }
            SessionEvent::SessionEnded { live_id, .. } => {
                if let Some(completed_segment_path) = contexts
                    .with_removed(&live_id, |state| {
                        let completed = state
                            .as_ref()
                            .and_then(|s| s.hls_ctx.as_ref().map(|ctx| ctx.path().clone()));
                        *state = None;
                        completed
                    })
                    .await
                    .flatten()
                {
                    Self::emit_segment_complete(live_id, completed_segment_path).await;
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn create_stream_temp_dir(stream_id: &str) -> Result<TempDir> {
        let sanitized = normalize_component(stream_id);
        let prefix = format!("livestream-rs-segment-{}-", sanitized);

        Ok(Builder::new().prefix(&prefix).tempdir()?)
    }

    async fn write_packet_for_stream(&self, stream_id: &str, mut packet: Packet) -> Result<()> {
        let completed_segment_path = self
            .contexts
            .with_or_create(stream_id, |state| -> Result<Option<PathBuf>> {
                if let Some(state) = state.as_mut() {
                    let mut completed: Option<PathBuf> = None;

                    if state.should_rollover(&packet, self.segment_duration) {
                        completed = Some(state.rollover()?);
                    }

                    if let Some(hls_ctx) = state.hls_ctx.as_mut() {
                        packet.rescale_ts_for_stream(state.streams.as_ref(), hls_ctx)?;
                        packet.write(hls_ctx)?;
                    }

                    Ok(completed)
                } else {
                    warn!(stream_id = %stream_id, "Uninitialized before packet write, ignored.");
                    Ok(None)
                }
            })
            .await?;

        if let Some(path) = completed_segment_path {
            Self::emit_segment_complete(stream_id.to_string(), path).await;
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl MiddlewareTrait for SegmentMiddleware {
    type Context = UnifiedPacketContext;

    async fn send(&self, ctx: Self::Context) -> Result<Self::Context> {
        let stream_id = ctx.id();

        match ctx.payload() {
            UnifiedPacket::AVPacket(packet) => {
                self.write_packet_for_stream(&stream_id, packet.clone())
                    .await?;
            }
            UnifiedPacket::FlvTag(tag) => {
                let maybe_packet: Option<Packet> = tag.clone().try_into().ok();

                if let Some(packet) = maybe_packet {
                    self.write_packet_for_stream(&stream_id, packet).await?;
                }
            }
        }

        Ok(ctx)
    }
}
