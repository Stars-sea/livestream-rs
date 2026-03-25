use std::sync::Arc;

use anyhow::Result;
use crossfire::{AsyncRx, MAsyncTx, MTx, mpsc, oneshot};
use tracing::{Span, instrument};

use self::actor::ManagerActor;
use self::command::ManagerCommand;
use super::stream_info::StreamInfo;
use crate::config::{EgressConfig, IngestConfig};
use crate::contracts::StreamRegistry;
use crate::ingest::PortAllocator;
use crate::ingest::adapters::RtmpTag;
use crate::ingest::events::StreamMessage;
use crate::media::output::FlvPacket;

mod actor;
mod command;
mod context;
mod state;

/// Public facade for stream lifecycle operations.
///
/// Responsibilities:
/// - Expose async APIs for callers (make/start/stop/list/get).
/// - Translate each API call into `ManagerCommand` and await typed replies.
///
/// Out of scope:
/// - No direct ownership or mutation of stream state.
/// - No protocol-specific ingest logic.
#[derive(Clone, Debug)]
pub struct StreamManager {
    cmd_tx: MAsyncTx<mpsc::Array<ManagerCommand>>,
}

impl StreamManager {
    pub fn new(
        ingest_config: IngestConfig,
        egress_config: EgressConfig,
        flv_packet_tx: MTx<mpsc::List<FlvPacket>>,
    ) -> (Self, AsyncRx<mpsc::List<(StreamMessage, Span)>>) {
        let (cmd_tx, cmd_rx) = mpsc::bounded_async(128);
        let (stream_msg_tx, stream_msg_rx) = mpsc::unbounded_async();

        let (start_port, end_port) = ingest_config
            .srt_port_range()
            .expect("Invalid SRT port range in settings");
        let port_allocator = Arc::new(PortAllocator::new(start_port, end_port));

        let actor = ManagerActor::new(
            cmd_rx,
            flv_packet_tx,
            stream_msg_tx.clone(),
            cmd_tx.clone(),
            port_allocator,
            ingest_config,
            egress_config,
        );
        tokio::spawn(actor.run());

        let manager = Self { cmd_tx };

        (manager, stream_msg_rx)
    }

    pub async fn make_srt_stream_info(
        &self,
        live_id: &str,
        passphrase: &str,
    ) -> Result<Arc<StreamInfo>> {
        let (reply, rx) = oneshot::oneshot();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::MakeSrtStreamInfo {
                live_id: live_id.to_string(),
                passphrase: passphrase.to_string(),
                reply,
            })
            .await;
        rx.await?
    }

    pub async fn make_rtmp_stream_info(&self, live_id: &str) -> Result<Arc<StreamInfo>> {
        let (reply, rx) = oneshot::oneshot();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::MakeRtmpStreamInfo {
                live_id: live_id.to_string(),
                reply,
            })
            .await;
        rx.await?
    }

    #[instrument(name = "ingest.manager.start", skip(self, stream_info), fields(stream.live_id = %stream_info.live_id()))]
    pub async fn start_srt_stream(self: &Arc<Self>, stream_info: Arc<StreamInfo>) -> Result<()> {
        let (reply, rx) = oneshot::oneshot();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::StartSrtStream {
                info: stream_info,
                reply,
            })
            .await;
        rx.await?
    }

    #[instrument(name = "ingest.manager.start_rtmp", skip(self, stream_info), fields(stream.live_id = %stream_info.live_id()))]
    pub async fn start_rtmp_stream(self: &Arc<Self>, stream_info: Arc<StreamInfo>) -> Result<()> {
        let (reply, rx) = oneshot::oneshot();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::StartRtmpStream {
                info: stream_info,
                reply,
            })
            .await;
        rx.await?
    }

    #[instrument(name = "ingest.manager.stop", skip(self), fields(stream.live_id = %live_id))]
    pub async fn stop_stream(&self, live_id: &str) -> Result<()> {
        let (reply, rx) = oneshot::oneshot();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::StopStream {
                live_id: live_id.to_string(),
                reply,
            })
            .await;
        rx.await?
    }

    #[instrument(name = "ingest.manager.shutdown", skip(self))]
    pub async fn shutdown(&self) {
        let (reply, rx) = oneshot::oneshot();
        let _ = self.cmd_tx.send(ManagerCommand::Shutdown { reply }).await;
        let _ = rx.await;
    }

    #[instrument(name = "ingest.manager.list_active", skip(self))]
    pub async fn list_active_streams(&self) -> Result<Vec<String>> {
        let (reply, rx) = oneshot::oneshot();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::ListActiveStreams { reply })
            .await;
        rx.await?
    }

    #[instrument(name = "ingest.manager.is_empty", skip(self))]
    pub async fn is_streams_empty(&self) -> bool {
        let (reply, rx) = oneshot::oneshot();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::IsStreamsEmpty { reply })
            .await;
        rx.await.unwrap_or(true)
    }

    #[allow(dead_code)]
    #[instrument(name = "ingest.manager.exists", skip(self), fields(stream.live_id = %live_id))]
    pub async fn has_stream(&self, live_id: &str) -> bool {
        let (reply, rx) = oneshot::oneshot();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::HasStream {
                live_id: live_id.to_string(),
                reply,
            })
            .await;
        rx.await.unwrap_or(false)
    }

    #[instrument(name = "ingest.manager.get_info", skip(self), fields(stream.live_id = %live_id))]
    pub async fn get_stream_info(&self, live_id: &str) -> Option<Arc<StreamInfo>> {
        let (reply, rx) = oneshot::oneshot();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::GetStreamInfo {
                live_id: live_id.to_string(),
                reply,
            })
            .await;
        rx.await.unwrap_or(None)
    }
}

#[async_trait::async_trait]
impl StreamRegistry for StreamManager {
    async fn get_stream(&self, live_id: &str) -> Option<Arc<StreamInfo>> {
        self.get_stream_info(live_id).await
    }

    async fn get_rtmp_tx(&self, live_id: &str) -> Option<MTx<mpsc::List<RtmpTag>>> {
        let (reply, rx) = oneshot::oneshot();
        let _ = self
            .cmd_tx
            .send(ManagerCommand::GetRtmpTx {
                live_id: live_id.to_string(),
                reply,
            })
            .await;
        rx.await.unwrap_or(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_manager_actor_flow() {
        let (cmd_tx, cmd_rx) = mpsc::bounded_async(10);
        let (flv_tx, _flv_rx) = mpsc::unbounded_async();
        let (msg_tx, _msg_rx) = mpsc::unbounded_async();

        let ingest_config = IngestConfig::default();
        let egress_config = EgressConfig::default();
        let (start_port, end_port) = ingest_config.srt_port_range().unwrap();
        let port_allocator = Arc::new(PortAllocator::new(start_port, end_port));

        let actor = ManagerActor::new(
            cmd_rx,
            flv_tx,
            msg_tx,
            cmd_tx.clone(),
            port_allocator,
            ingest_config,
            egress_config,
        );
        tokio::spawn(actor.run());

        let manager = StreamManager { cmd_tx };

        let streams = manager.list_active_streams().await.unwrap();
        assert!(streams.is_empty());
        assert!(manager.is_streams_empty().await);

        let info = manager.make_rtmp_stream_info("test_live").await.unwrap();
        assert_eq!(info.live_id(), "test_live");

        assert!(manager.has_stream("test_live").await);
        let active = manager.list_active_streams().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0], "test_live");

        manager.stop_stream("test_live").await.unwrap();
        assert!(!manager.has_stream("test_live").await);

        manager.shutdown().await;
    }

    #[tokio::test]
    async fn test_repeated_stop_start_same_live_id() {
        let (cmd_tx, cmd_rx) = mpsc::bounded_async(10);
        let (flv_tx, _flv_rx) = mpsc::unbounded_async();
        let (msg_tx, _msg_rx) = mpsc::unbounded_async();

        let ingest_config = IngestConfig::default();
        let egress_config = EgressConfig::default();
        let (start_port, end_port) = ingest_config.srt_port_range().unwrap();
        let port_allocator = Arc::new(PortAllocator::new(start_port, end_port));

        let actor = ManagerActor::new(
            cmd_rx,
            flv_tx,
            msg_tx,
            cmd_tx.clone(),
            port_allocator,
            ingest_config,
            egress_config,
        );
        tokio::spawn(actor.run());

        let manager = StreamManager { cmd_tx };

        let info1 = manager.make_rtmp_stream_info("live_repeat").await.unwrap();
        assert_eq!(info1.live_id(), "live_repeat");

        manager.stop_stream("live_repeat").await.unwrap();
        assert!(!manager.has_stream("live_repeat").await);

        let info2 = manager.make_rtmp_stream_info("live_repeat").await.unwrap();
        assert_eq!(info2.live_id(), "live_repeat");
        assert!(manager.has_stream("live_repeat").await);

        manager.stop_stream("live_repeat").await.unwrap();
        assert!(!manager.has_stream("live_repeat").await);
    }

    #[tokio::test]
    async fn test_quick_recreate_after_early_stop() {
        let (cmd_tx, cmd_rx) = mpsc::bounded_async(10);
        let (flv_tx, _flv_rx) = mpsc::unbounded_async();
        let (msg_tx, _msg_rx) = mpsc::unbounded_async();

        let ingest_config = IngestConfig::default();
        let egress_config = EgressConfig::default();
        let (start_port, end_port) = ingest_config.srt_port_range().unwrap();
        let port_allocator = Arc::new(PortAllocator::new(start_port, end_port));

        let actor = ManagerActor::new(
            cmd_rx,
            flv_tx,
            msg_tx,
            cmd_tx.clone(),
            port_allocator,
            ingest_config,
            egress_config,
        );
        tokio::spawn(actor.run());

        let manager = StreamManager { cmd_tx };

        manager.make_rtmp_stream_info("live_quick").await.unwrap();
        manager.stop_stream("live_quick").await.unwrap();
        assert!(!manager.has_stream("live_quick").await);

        manager.make_rtmp_stream_info("live_quick").await.unwrap();
        assert!(manager.has_stream("live_quick").await);

        manager.stop_stream("live_quick").await.unwrap();
    }

    #[tokio::test]
    async fn test_quick_recreate_after_ingest_start_error() {
        let (cmd_tx, cmd_rx) = mpsc::bounded_async(10);
        let (flv_tx, _flv_rx) = mpsc::unbounded_async();
        let (msg_tx, _msg_rx) = mpsc::unbounded_async();

        let ingest_config = IngestConfig::default();
        let egress_config = EgressConfig::default();
        let (start_port, end_port) = ingest_config.srt_port_range().unwrap();
        let port_allocator = Arc::new(PortAllocator::new(start_port, end_port));

        let actor = ManagerActor::new(
            cmd_rx,
            flv_tx,
            msg_tx,
            cmd_tx.clone(),
            port_allocator,
            ingest_config,
            egress_config,
        );
        tokio::spawn(actor.run());

        let manager = Arc::new(StreamManager { cmd_tx });

        let info = manager.make_rtmp_stream_info("live_error").await.unwrap();
        let start_result = manager.start_srt_stream(info).await;
        assert!(start_result.is_err());

        manager.stop_stream("live_error").await.unwrap();
        assert!(!manager.has_stream("live_error").await);

        let recreated = manager.make_rtmp_stream_info("live_error").await;
        assert!(recreated.is_ok());
    }
}
