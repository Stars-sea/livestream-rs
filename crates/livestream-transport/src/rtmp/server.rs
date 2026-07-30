use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use dashmap::DashMap;
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use super::connection::RtmpConnection;
use crate::config::ServerConfig;
use crate::flv::FlvEgressHub;
use crate::lifecycle::HandlerLifecycle;
use crate::protocol_server::ProtocolServerCore;
use crate::registry::SessionRegistry;
use crate::rtmp::handler::HandlerBuilder;
use crate::source::rtmp::RtmpSource;
use livestream_codec::EncodedPacket;
use livestream_codec::SegmentConfig;
use livestream_core::pad::PadSender;
use livestream_core::traits::{Node, Source};
use livestream_core::types::Protocol;

use livestream_pipeline::factory;
use livestream_pipeline::sink::minio::ObjectUploader;

pub struct RtmpServer {
    core: ProtocolServerCore,
    appname: String,
}

impl RtmpServer {
    pub async fn create(cfg: ServerConfig, appname: String) -> Result<Self> {
        let core = ProtocolServerCore::from_config(cfg).await?;
        Ok(Self { core, appname })
    }

    pub async fn run(mut self) -> Result<()> {
        let appname = self.appname.clone();
        let pending = self.core.pending_lifecycle.clone();
        let hub = self.core.flv_egress_hub.clone();
        let minio = self.core.minio.clone();
        let seg_cfg = self.core.segment_cfg.clone();
        let registry = self.core.registry.clone();

        self.core
            .run(Protocol::Rtmp, move |socket, addr| {
                debug!(client_addr = %addr, "Accepted new RTMP connection");
                Box::pin(spawn_connection_handler(
                    appname.clone(),
                    socket,
                    pending.clone(),
                    hub.clone(),
                    minio.clone(),
                    seg_cfg.clone(),
                    registry.clone(),
                ))
            })
            .await
    }
}

async fn spawn_connection_handler(
    appname: String,
    socket: TcpStream,
    pending_lifecycle: Arc<DashMap<String, HandlerLifecycle>>,
    flv_egress_hub: Arc<FlvEgressHub>,
    minio: Arc<dyn ObjectUploader>,
    segment_cfg: SegmentConfig,
    registry: Arc<SessionRegistry>,
) {
    let cancel_token = CancellationToken::new();
    let _cancel_guard = cancel_token.drop_guard_ref();

    let connection = RtmpConnection::new(socket);

    let builder = match connection.perform_handshake(&cancel_token).await {
        Ok(builder) => builder,
        Err(e) => {
            warn!(error = %e, "RTMP handshake failed");
            return;
        }
    };

    let builder = builder
        .with_appname(appname)
        .with_registry(registry.clone());
    let session = match builder.build() {
        Ok(session) => session,
        Err(e) => {
            warn!(error = %e, "Failed to build RTMP session guard");
            return;
        }
    };

    let builder = match session.connect(&pending_lifecycle, &cancel_token).await {
        Ok(builder) => builder,
        Err(e) => {
            warn!(error = %e, "Failed to connect RTMP session");
            return;
        }
    };

    drop(_cancel_guard);

    let stream_key = builder.stream_key().to_string();
    let is_publish = matches!(&builder, HandlerBuilder::Publish { .. });

    let Some(cancel_token) = registry.get_cancel_token(&stream_key) else {
        error!(stream_key = %stream_key, "No cancellation token found for stream key");
        return;
    };

    // For publish, the registry token is cancelled when we exit so the
    // session lifecycle cleanup fires. We track this explicitly rather
    // than via DropGuard so we can control cancel-vs-drain ordering.
    let publish_token = is_publish.then(|| cancel_token.clone());

    let builder = if is_publish {
        let Some((_, lifecycle)) = pending_lifecycle.remove(&stream_key) else {
            warn!(stream_key = %stream_key, "No pending lifecycle found, exiting...");
            return;
        };
        // Build the pipeline via PipelineFactory.
        let pipeline_cancel = cancel_token.child_token();
        let (src_tx, src_rx) = PadSender::<EncodedPacket>::new_channel(512);

        let (rtmp_source, frame_tx) =
            RtmpSource::new(&stream_key, vec![], src_tx, pipeline_cancel.clone());
        let source = Arc::new(rtmp_source);

        // Spawn source
        spawn_source_task(source);

        // Build pipeline via PipelineFactory.
        // Only the FLV path (OTelProbe → SeqCacheProbe → FlvMux → FlvSink) is
        // built here; HLS is deferred until codec params arrive in-band (RTMP).
        match factory::build_pipeline(
            &stream_key,
            src_rx,
            &[],
            flv_egress_hub.clone(),
            minio,
            &segment_cfg,
            pipeline_cancel,
        ) {
            Ok(_pipeline) => {}
            Err(e) => {
                warn!(stream_key = %stream_key, error = %e, "Pipeline construction failed");
                return;
            }
        }
        builder.with_lifecycle(lifecycle).with_source_tx(frame_tx)
    } else {
        let Some((tag_stream, cached_tags)) = flv_egress_hub.subscribe(&stream_key) else {
            warn!(stream_key = %stream_key, "No FLV channel found for play request");
            return;
        };
        builder
            .with_tag_stream(tag_stream)
            .with_cached_tags(cached_tags)
    };

    let builder = builder.with_cancel_token(cancel_token.clone());

    match builder.build() {
        Ok(mut handler) => {
            if let Err(e) = handler.handle().await {
                warn!(error = %e, "Error handling RTMP session");
            }
        }
        Err(e) => {
            warn!(error = %e, "Failed to build RTMP session handler");
        }
    }

    // Cancel the pipeline before removing the FLV channel so buffered
    // FlvTags drain to subscribers before the channel is destroyed.
    cancel_token.cancel();

    // Allow buffered pipeline data to drain before removing the channel.
    tokio::time::sleep(Duration::from_millis(100)).await;

    flv_egress_hub.remove_channel(&stream_key);

    // Ensure registry token is cancelled for publish sessions.
    drop(publish_token);
}

fn spawn_source_task(source: Arc<RtmpSource>) {
    tokio::spawn(async move {
        if let Err(e) = source.start().await {
            tracing::error!(stream = %source.name(), error = %e, "RtmpSource failed");
        }
    });
}
