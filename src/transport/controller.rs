use anyhow::Result;

use crate::channel::{MpscTx, SendError};

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

    pub fn precreate_rtmp_session(&self, live_id: String) -> Result<()> {
        let channel = self.rtmp_channel.clone().with_live_id(live_id.clone());

        let msg = ControlMessage::PrecreateStream {
            live_id,
            passphrase: None,
        };
        match channel.send(msg) {
            Ok(()) => Ok(()),
            Err(SendError::Full) => anyhow::bail!("RTMP control queue is full"),
            Err(SendError::Closed) => {
                anyhow::bail!("RTMP control queue is disconnected")
            }
        }
    }

    pub fn precreate_srt_session(&self, live_id: String, passphrase: String) -> Result<()> {
        let channel = self.srt_channel.clone().with_live_id(live_id.clone());

        let msg = ControlMessage::PrecreateStream {
            live_id,
            passphrase: Some(passphrase),
        };
        match channel.send(msg) {
            Ok(()) => Ok(()),
            Err(SendError::Full) => anyhow::bail!("SRT control queue is full"),
            Err(SendError::Closed) => {
                anyhow::bail!("SRT control queue is disconnected")
            }
        }
    }

    pub fn close_session(&self, live_id: String) -> Result<()> {
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

        Ok(())
    }
}
