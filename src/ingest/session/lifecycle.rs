use std::path::PathBuf;
use crossfire::{MTx, mpsc};
use tracing::Span;

use crate::media::output::FlvPacket;

use crate::ingest::events::StreamMessage;

/// Internal lifecycle phase marker for one ingest worker session.
///
/// Responsibilities:
/// - Represent coarse worker phase transitions used by `WorkerLifecycle` guards.
/// - Keep notification sequencing predictable during start/stop/finalize.
///
/// Out of scope:
/// - Not a global stream manager state machine.
/// - Not a transport/protocol status model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkerSessionState {
    Created,
    Running,
    Stopping,
    Stopped,
}

/// Per-worker-session lifecycle event guard.
///
/// Responsibilities:
/// - De-duplicate lifecycle notifications emitted from one worker session.
/// - Track worker progression (`Created` -> `Running` -> `Stopping` -> `Stopped`).
/// - Emit worker-level start/stop and EOS exactly once for the session.
///
/// Out of scope:
/// - No protocol-level ingest logic.
/// - No ownership of global stream registry or resources.
/// - Should not define business retry policy.
#[derive(Debug, Clone)]
pub struct WorkerLifecycle {
    live_id: String,
    stream_msg_tx: MTx<mpsc::List<(StreamMessage, Span)>>,
    state: WorkerSessionState,
    stream_started_notified: bool,
    stream_stopped_notified: bool,
    worker_started_notified: bool,
    worker_stopped_notified: bool,
    eos_sent: bool,
}

impl WorkerLifecycle {
    pub fn new(live_id: &str, stream_msg_tx: MTx<mpsc::List<(StreamMessage, Span)>>) -> Self {
        Self {
            live_id: live_id.to_string(),
            stream_msg_tx,
            state: WorkerSessionState::Created,
            stream_started_notified: false,
            stream_stopped_notified: false,
            worker_started_notified: false,
            worker_stopped_notified: false,
            eos_sent: false,
        }
    }

    pub fn notify_ingest_worker_started(&mut self) {
        if self.worker_started_notified {
            return;
        }

        self.emit_ingest_worker_started();
        self.worker_started_notified = true;
        if self.state == WorkerSessionState::Created {
            self.state = WorkerSessionState::Running;
        }
    }

    pub fn notify_stream_started(&mut self) {
        if self.stream_started_notified && !self.stream_stopped_notified {
            return;
        }

        // A new stream cycle can start again in the same worker after a prior stop.
        if self.stream_started_notified && self.stream_stopped_notified {
            self.stream_started_notified = false;
            self.stream_stopped_notified = false;
        }

        self.emit_stream_started();
        self.stream_started_notified = true;
        if self.state == WorkerSessionState::Created {
            self.state = WorkerSessionState::Running;
        }
    }

    pub fn notify_stream_stopped(&mut self, error: Option<String>) {
        if !self.stream_started_notified || self.stream_stopped_notified {
            return;
        }

        self.emit_stream_stopped(error);
        self.stream_stopped_notified = true;
        if self.state != WorkerSessionState::Stopped {
            self.state = WorkerSessionState::Stopping;
        }
    }

    pub fn notify_stream_restarting(&self, error: String) {
        self.emit_stream_restarting(error);
    }

    pub fn notify_segment_complete(&self, path: &PathBuf) {
        self.emit_segment_complete(path);
    }

    pub fn send_end_of_stream_once(&mut self, flv_packet_tx: &MTx<mpsc::List<FlvPacket>>) {
        if self.eos_sent {
            return;
        }

        self.emit_end_of_stream(flv_packet_tx);
        self.eos_sent = true;
    }

    pub fn notify_ingest_worker_stopped(&mut self) {
        if self.worker_stopped_notified {
            return;
        }

        self.emit_ingest_worker_stopped();
        self.worker_stopped_notified = true;
        self.state = WorkerSessionState::Stopped;
    }

    pub fn finalize(
        &mut self,
        flv_packet_tx: &MTx<mpsc::List<FlvPacket>>,
        error: Option<String>,
    ) {
        self.notify_stream_stopped(error);
        self.send_end_of_stream_once(flv_packet_tx);
        self.notify_ingest_worker_stopped();
    }

    fn emit_ingest_worker_started(&self) {
        let _ = self.stream_msg_tx.send((
            StreamMessage::ingest_worker_started(&self.live_id),
            Span::current(),
        ));
    }

    fn emit_ingest_worker_stopped(&self) {
        let _ = self.stream_msg_tx.send((
            StreamMessage::ingest_worker_stopped(&self.live_id),
            Span::current(),
        ));
    }

    fn emit_stream_started(&self) {
        let _ = self.stream_msg_tx.send((
            StreamMessage::stream_started(&self.live_id),
            Span::current(),
        ));
    }

    fn emit_stream_stopped(&self, error: Option<String>) {
        let _ = self.stream_msg_tx.send((
            StreamMessage::stream_stopped(&self.live_id, error),
            Span::current(),
        ));
    }

    fn emit_stream_restarting(&self, error: String) {
        let _ = self.stream_msg_tx.send((
            StreamMessage::stream_restarting(&self.live_id, error),
            Span::current(),
        ));
    }

    fn emit_segment_complete(&self, path: &PathBuf) {
        let _ = self.stream_msg_tx.send((
            StreamMessage::segment_complete(&self.live_id, path),
            Span::current(),
        ));
    }

    fn emit_end_of_stream(&self, flv_packet_tx: &MTx<mpsc::List<FlvPacket>>) {
        let _ = flv_packet_tx.send(FlvPacket::EndOfStream {
            live_id: self.live_id.to_string(),
        });
    }
}
