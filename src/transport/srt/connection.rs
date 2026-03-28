use crossfire::{MTx, mpsc::List};
use tokio_util::sync::CancellationToken;

use crate::transport::message::StreamEvent;

pub struct SrtConnection {
    live_id: String,

    event_tx: MTx<List<StreamEvent>>,

    cancel_token: CancellationToken,
}

impl SrtConnection {
    pub fn new(
        live_id: String,
        event_tx: MTx<List<StreamEvent>>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            live_id,
            event_tx,
            cancel_token,
        }
    }

    pub fn run(self) {}
}
