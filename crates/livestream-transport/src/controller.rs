use std::future::Future;
use std::time::Duration;

use anyhow::Result;
use crossfire::oneshot::{RxOneshot, oneshot};
use livestream_core::channel::{MpscTx, SendError};

use crate::registry;
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
    rtmp_channel: MpscTx<ControlMessage>,
}

impl TransportController {
    pub fn new(rtmp_channel: MpscTx<ControlMessage>) -> Self {
        Self { rtmp_channel }
    }

    pub fn precreate_rtmp_session(
        &self,
        live_id: String,
    ) -> Result<RxOneshot<Result<SessionDescriptor>>> {
        self.precreate_session(self.rtmp_channel.clone(), "RTMP", live_id, None)
    }

    pub fn close_session(&self, live_id: String) -> Result<RxOneshot<Result<()>>> {
        let live_id_clone = live_id.clone();
        let rx = Self::spawn_waiter(async move {
            Self::wait_for_cleanup(&live_id_clone, SESSION_CLEANUP_TIMEOUT).await
        });

        let msg = ControlMessage::StopStream {
            live_id: live_id.clone(),
        };

        let rtmp_channel = self.rtmp_channel.clone().with_live_id(live_id);

        let rtmp_status = rtmp_channel.send(msg);

        if rtmp_status.is_err() {
            anyhow::bail!("Failed to send StopStream to RTMP: {:?}", rtmp_status);
        }

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
        let rx = Self::spawn_waiter(async move {
            Self::wait_for_descriptor(&live_id_clone, DESCRIPTOR_READY_TIMEOUT).await
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

    async fn wait_for_descriptor(live_id: &str, timeout: Duration) -> Result<SessionDescriptor> {
        let live_id = live_id.to_owned();
        tokio::time::timeout(timeout, Self::poll_descriptor(&live_id))
            .await
            .map_err(|_| anyhow::anyhow!("Timeout while waiting for stream descriptor"))
    }

    async fn poll_descriptor(live_id: &str) -> SessionDescriptor {
        loop {
            if let Some(descriptor) = registry::INSTANCE.get_descriptor(live_id).await {
                return descriptor;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn wait_for_cleanup(live_id: &str, timeout: Duration) -> Result<()> {
        let live_id = live_id.to_owned();
        tokio::time::timeout(timeout, Self::poll_cleanup(&live_id))
            .await
            .map_err(|_| anyhow::anyhow!("Timeout while waiting for stream cleanup"))
    }

    async fn poll_cleanup(live_id: &str) {
        loop {
            if registry::INSTANCE.get_descriptor(live_id).await.is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
