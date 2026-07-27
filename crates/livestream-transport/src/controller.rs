use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossfire::oneshot::{RxOneshot, oneshot};
use livestream_core::channel::{MpscTx, SendError};
use livestream_core::types::Protocol;

use crate::registry::SessionRegistry;
use crate::registry::state::SessionDescriptor;

const DESCRIPTOR_READY_TIMEOUT: Duration = Duration::from_secs(2);
const SESSION_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub enum ControlMessage {
    PrecreateStream {
        live_id: String,
        passphrase: Option<String>,
    },

    StopStream {
        live_id: String,
    },
}

pub struct TransportController {
    registry: Arc<SessionRegistry>,
    rtmp_channel: MpscTx<ControlMessage>,
    rtsp_channel: MpscTx<ControlMessage>,
}

impl TransportController {
    pub fn new(
        registry: Arc<SessionRegistry>,
        rtmp_channel: MpscTx<ControlMessage>,
        rtsp_channel: MpscTx<ControlMessage>,
    ) -> Self {
        Self {
            registry,
            rtmp_channel,
            rtsp_channel,
        }
    }

    pub fn precreate_rtmp_session(
        &self,
        live_id: String,
    ) -> Result<RxOneshot<Result<SessionDescriptor>>> {
        self.precreate_session(self.rtmp_channel.clone(), "RTMP", live_id, None)
    }

    pub fn precreate_rtsp_session(
        &self,
        live_id: String,
        passphrase: Option<String>,
    ) -> Result<RxOneshot<Result<SessionDescriptor>>> {
        self.precreate_session(self.rtsp_channel.clone(), "RTSP", live_id, passphrase)
    }

    pub fn close_session(&self, live_id: String) -> Result<RxOneshot<Result<()>>> {
        let rtmp = self.rtmp_channel.clone();
        let rtsp = self.rtsp_channel.clone();
        let registry = self.registry.clone();
        let rx = Self::spawn_waiter(async move {
            let desc = registry.get_descriptor(&live_id).await;
            let Some(desc) = desc else {
                return Ok(());
            };
            let channel = match desc.protocol {
                Protocol::Rtmp => rtmp.with_live_id(live_id.clone()),
                Protocol::Rtsp => rtsp.with_live_id(live_id.clone()),
                _ => return Ok(()),
            };
            let msg = ControlMessage::StopStream {
                live_id: live_id.clone(),
            };
            if let Err(e) = channel.send(msg) {
                tracing::warn!(
                    live_id = %live_id,
                    error = %e,
                    "TransportController: failed to send StopStream control message"
                );
            }
            wait_for_cleanup(&registry, &live_id, SESSION_CLEANUP_TIMEOUT).await
        });
        Ok(rx)
    }

    fn precreate_session(
        &self,
        channel: MpscTx<ControlMessage>,
        transport_name: &'static str,
        live_id: String,
        passphrase: Option<String>,
    ) -> Result<RxOneshot<Result<SessionDescriptor>>> {
        let live_id_clone = live_id.clone();
        let registry = self.registry.clone();
        let rx = Self::spawn_waiter(async move {
            wait_for_descriptor(&registry, &live_id_clone, DESCRIPTOR_READY_TIMEOUT).await
        });

        let channel = channel.with_live_id(live_id.clone());
        let msg = ControlMessage::PrecreateStream {
            live_id,
            passphrase,
        };

        match channel.send(msg) {
            Ok(()) => Ok(rx),
            Err(err) => Err(Self::map_control_send_error(transport_name, err)),
        }
    }

    fn spawn_waiter<T, F>(future: F) -> RxOneshot<Result<T>>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        let (tx, rx) = oneshot();
        tokio::spawn(async move {
            let res = future.await;
            tx.send(res);
        });
        rx
    }

    fn map_control_send_error(transport_name: &'static str, err: SendError) -> anyhow::Error {
        match err {
            SendError::Full => anyhow::anyhow!("{transport_name} control queue is full"),
            SendError::Closed => anyhow::anyhow!("{transport_name} control queue is disconnected"),
        }
    }
}

async fn wait_for_descriptor(
    registry: &SessionRegistry,
    live_id: &str,
    timeout: Duration,
) -> Result<SessionDescriptor> {
    let live_id = live_id.to_owned();
    tokio::time::timeout(timeout, poll_descriptor(registry, &live_id))
        .await
        .map_err(|_| anyhow::anyhow!("Timeout while waiting for stream descriptor"))
}

async fn poll_descriptor(registry: &SessionRegistry, live_id: &str) -> SessionDescriptor {
    loop {
        if let Some(descriptor) = registry.get_descriptor(live_id).await {
            return descriptor;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_cleanup(
    registry: &SessionRegistry,
    live_id: &str,
    timeout: Duration,
) -> Result<()> {
    let live_id = live_id.to_owned();
    tokio::time::timeout(timeout, poll_cleanup(registry, &live_id))
        .await
        .map_err(|_| anyhow::anyhow!("Timeout while waiting for stream cleanup"))
}

async fn poll_cleanup(registry: &SessionRegistry, live_id: &str) {
    loop {
        if registry.get_descriptor(live_id).await.is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
