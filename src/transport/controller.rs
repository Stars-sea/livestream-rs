use std::future::Future;
use std::time::Duration;

use anyhow::Result;
use crossfire::oneshot::{RxOneshot, oneshot};

use crate::channel::{MpscTx, SendError};
use crate::transport::registry::{self, state::SessionDescriptor};

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
    srt_channel: MpscTx<ControlMessage>,
}

impl TransportController {
    pub fn new(rtmp_channel: MpscTx<ControlMessage>, srt_channel: MpscTx<ControlMessage>) -> Self {
        Self {
            rtmp_channel,
            srt_channel,
        }
    }

    pub fn precreate_rtmp_session(
        &self,
        live_id: String,
    ) -> Result<RxOneshot<Result<SessionDescriptor>>> {
        self.precreate_session(self.rtmp_channel.clone(), "RTMP", live_id, None)
    }

    pub fn precreate_srt_session(
        &self,
        live_id: String,
        passphrase: String,
    ) -> Result<RxOneshot<Result<SessionDescriptor>>> {
        self.precreate_session(self.srt_channel.clone(), "SRT", live_id, Some(passphrase))
    }

    pub fn close_session(&self, live_id: String) -> Result<RxOneshot<Result<()>>> {
        let live_id_clone = live_id.clone();
        let rx = Self::spawn_waiter(async move {
            Self::wait_for_cleanup(&live_id_clone, SESSION_CLEANUP_TIMEOUT).await
        });

        let msg = ControlMessage::StopStream {
            live_id: live_id.clone(),
        };

        let rtmp_channel = self.rtmp_channel.clone().with_live_id(live_id.clone());

        let srt_channel = self.srt_channel.clone().with_live_id(live_id);

        let rtmp_status = rtmp_channel.send(msg.clone());
        let srt_status = srt_channel.send(msg);

        if !matches!(rtmp_status, Ok(())) && !matches!(srt_status, Ok(())) {
            anyhow::bail!(
                "Failed to send StopStream to both RTMP and SRT: rtmp={:?}, srt={:?}",
                rtmp_status,
                srt_status
            );
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
        tokio::time::timeout(timeout, async move {
            loop {
                if let Some(descriptor) = registry::INSTANCE.get_descriptor(live_id).await {
                    return descriptor;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("Timeout while waiting for stream descriptor"))
    }

    async fn wait_for_cleanup(live_id: &str, timeout: Duration) -> Result<()> {
        tokio::time::timeout(timeout, async move {
            loop {
                if registry::INSTANCE.get_descriptor(live_id).await.is_none() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("Timeout while waiting for stream cleanup"))
    }
}
