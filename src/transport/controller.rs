use anyhow::Result;

use super::contract::message::ControlMessage;
use crate::queue::{ChannelSendStatus, MpscChannel};

pub struct TransportController {
    rtmp_channel: MpscChannel<ControlMessage>,
    srt_channel: MpscChannel<ControlMessage>,
}

impl TransportController {
    pub fn new(
        rtmp_channel: MpscChannel<ControlMessage>,
        srt_channel: MpscChannel<ControlMessage>,
    ) -> Self {
        Self {
            rtmp_channel,
            srt_channel,
        }
    }

    pub fn precreate_rtmp_session(&self, live_id: String) -> Result<()> {
        let channel = self
            .rtmp_channel
            .clone()
            .with_source("transport.controller.precreate_rtmp_session")
            .with_live_id(live_id.clone());

        let msg = ControlMessage::PrecreateStream {
            live_id,
            passphrase: None,
        };
        match channel.send(msg) {
            ChannelSendStatus::Sent => Ok(()),
            ChannelSendStatus::Full => anyhow::bail!("RTMP control queue is full"),
            ChannelSendStatus::Disconnected => {
                anyhow::bail!("RTMP control queue is disconnected")
            }
        }
    }

    pub fn precreate_srt_session(&self, live_id: String, passphrase: String) -> Result<()> {
        let channel = self
            .srt_channel
            .clone()
            .with_source("transport.controller.precreate_srt_session")
            .with_live_id(live_id.clone());

        let msg = ControlMessage::PrecreateStream {
            live_id,
            passphrase: Some(passphrase),
        };
        match channel.send(msg) {
            ChannelSendStatus::Sent => Ok(()),
            ChannelSendStatus::Full => anyhow::bail!("SRT control queue is full"),
            ChannelSendStatus::Disconnected => {
                anyhow::bail!("SRT control queue is disconnected")
            }
        }
    }

    pub fn close_session(&self, live_id: String) -> Result<()> {
        let msg = ControlMessage::StopStream {
            live_id: live_id.clone(),
        };

        let rtmp_channel = self
            .rtmp_channel
            .clone()
            .with_source("transport.controller.close_session:rtmp")
            .with_live_id(live_id.clone());

        let srt_channel = self
            .srt_channel
            .clone()
            .with_source("transport.controller.close_session:srt")
            .with_live_id(live_id);

        let rtmp_status = rtmp_channel.send(msg.clone());
        let srt_status = srt_channel.send(msg);

        if !matches!(rtmp_status, ChannelSendStatus::Sent)
            && !matches!(srt_status, ChannelSendStatus::Sent)
        {
            anyhow::bail!(
                "Failed to send StopStream to both RTMP and SRT: rtmp={:?}, srt={:?}",
                rtmp_status,
                srt_status
            );
        }

        Ok(())
    }
}
