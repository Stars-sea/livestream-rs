use anyhow::Result;
use crossfire::{Tx, spsc::Array};

use super::contract::message::ControlMessage;

pub struct TransportController {
    rtmp_tx: Tx<Array<ControlMessage>>,
    srt_tx: Tx<Array<ControlMessage>>,
}

impl TransportController {
    pub fn new(rtmp_tx: Tx<Array<ControlMessage>>, srt_tx: Tx<Array<ControlMessage>>) -> Self {
        Self { rtmp_tx, srt_tx }
    }

    pub fn precreate_rtmp_session(&self, live_id: String) -> Result<()> {
        let msg = ControlMessage::PrecreateStream {
            live_id,
            passphrase: None,
        };
        self.rtmp_tx.send(msg)?;
        Ok(())
    }

    pub fn precreate_srt_session(&self, live_id: String, passphrase: String) -> Result<()> {
        let msg = ControlMessage::PrecreateStream {
            live_id,
            passphrase: Some(passphrase),
        };
        self.srt_tx.send(msg)?;
        Ok(())
    }

    pub fn close_session(&self, live_id: String) -> Result<()> {
        let msg = ControlMessage::StopStream {
            live_id: live_id.clone(),
        };

        let rtmp_err = self.rtmp_tx.send(msg.clone()).err();
        let srt_err = self.srt_tx.send(msg).err();

        if let (Some(rtmp_err), Some(srt_err)) = (rtmp_err, srt_err) {
            anyhow::bail!(
                "Failed to send StopStream to both RTMP and SRT: rtmp={}, srt={}",
                rtmp_err,
                srt_err
            );
        }

        Ok(())
    }
}
