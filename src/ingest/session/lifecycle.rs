use tokio::sync::mpsc;
use tracing::Span;

use crate::media::output::FlvPacket;

use crate::ingest::events::StreamMessage;

/// Tracks the internal execution state machine of a worker managing an ingest stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkerState {
    Created,
    Running,
    Stopping,
    Stopped,
}

/// Encapsulates the tracking and dispatching logic for stream life-cycle events.
/// Ensures that transitions like start, stop, or End of Stream (EOS) are emitted exactly once
/// per worker session, eliminating duplicated event triggering.
#[derive(Debug)]
pub struct WorkerLifecycle {
    live_id: String,
    state: WorkerState,
    stream_started_notified: bool,
    stream_stopped_notified: bool,
    worker_started_notified: bool,
    worker_stopped_notified: bool,
    eos_sent: bool,
}

impl WorkerLifecycle {
    pub fn new(live_id: &str) -> Self {
        Self {
            live_id: live_id.to_string(),
            state: WorkerState::Created,
            stream_started_notified: false,
            stream_stopped_notified: false,
            worker_started_notified: false,
            worker_stopped_notified: false,
            eos_sent: false,
        }
    }

    pub fn notify_ingest_worker_started(
        &mut self,
        stream_msg_tx: &mpsc::UnboundedSender<(StreamMessage, Span)>,
    ) {
        if self.worker_started_notified {
            return;
        }

        notify_ingest_worker_started(stream_msg_tx, &self.live_id);
        self.worker_started_notified = true;
        if self.state == WorkerState::Created {
            self.state = WorkerState::Running;
        }
    }

    pub fn notify_stream_started(
        &mut self,
        stream_msg_tx: &mpsc::UnboundedSender<(StreamMessage, Span)>,
    ) {
        if self.stream_started_notified {
            return;
        }

        notify_stream_started(stream_msg_tx, &self.live_id);
        self.stream_started_notified = true;
        if self.state == WorkerState::Created {
            self.state = WorkerState::Running;
        }
    }

    pub fn notify_stream_stopped(
        &mut self,
        stream_msg_tx: &mpsc::UnboundedSender<(StreamMessage, Span)>,
        error: Option<String>,
    ) {
        if !self.stream_started_notified || self.stream_stopped_notified {
            return;
        }

        notify_stream_stopped(stream_msg_tx, &self.live_id, error);
        self.stream_stopped_notified = true;
        if self.state != WorkerState::Stopped {
            self.state = WorkerState::Stopping;
        }
    }

    #[allow(dead_code)]
    pub fn notify_stream_restarting(
        &self,
        stream_msg_tx: &mpsc::UnboundedSender<(StreamMessage, Span)>,
        error: String,
    ) {
        notify_stream_restarting(stream_msg_tx, &self.live_id, error);
    }

    pub fn send_end_of_stream_once(&mut self, flv_packet_tx: &mpsc::UnboundedSender<FlvPacket>) {
        if self.eos_sent {
            return;
        }

        send_end_of_stream(flv_packet_tx, &self.live_id);
        self.eos_sent = true;
    }

    pub fn notify_ingest_worker_stopped(
        &mut self,
        stream_msg_tx: &mpsc::UnboundedSender<(StreamMessage, Span)>,
    ) {
        if self.worker_stopped_notified {
            return;
        }

        notify_ingest_worker_stopped(stream_msg_tx, &self.live_id);
        self.worker_stopped_notified = true;
        self.state = WorkerState::Stopped;
    }

    pub fn finalize(
        &mut self,
        stream_msg_tx: &mpsc::UnboundedSender<(StreamMessage, Span)>,
        flv_packet_tx: &mpsc::UnboundedSender<FlvPacket>,
        error: Option<String>,
    ) {
        self.notify_stream_stopped(stream_msg_tx, error);
        self.send_end_of_stream_once(flv_packet_tx);
        self.notify_ingest_worker_stopped(stream_msg_tx);
    }
}

pub(super) fn notify_ingest_worker_started(
    stream_msg_tx: &mpsc::UnboundedSender<(StreamMessage, Span)>,
    live_id: &str,
) {
    let _ = stream_msg_tx.send((
        StreamMessage::ingest_worker_started(live_id),
        Span::current(),
    ));
}

pub(super) fn notify_ingest_worker_stopped(
    stream_msg_tx: &mpsc::UnboundedSender<(StreamMessage, Span)>,
    live_id: &str,
) {
    let _ = stream_msg_tx.send((
        StreamMessage::ingest_worker_stopped(live_id),
        Span::current(),
    ));
}

pub(super) fn notify_stream_started(
    stream_msg_tx: &mpsc::UnboundedSender<(StreamMessage, Span)>,
    live_id: &str,
) {
    let _ = stream_msg_tx.send((StreamMessage::stream_started(live_id), Span::current()));
}

pub(super) fn notify_stream_stopped(
    stream_msg_tx: &mpsc::UnboundedSender<(StreamMessage, Span)>,
    live_id: &str,
    error: Option<String>,
) {
    let _ = stream_msg_tx.send((
        StreamMessage::stream_stopped(live_id, error),
        Span::current(),
    ));
}

#[allow(dead_code)]
pub(super) fn notify_stream_restarting(
    stream_msg_tx: &mpsc::UnboundedSender<(StreamMessage, Span)>,
    live_id: &str,
    error: String,
) {
    let _ = stream_msg_tx.send((
        StreamMessage::stream_restarting(live_id, error),
        Span::current(),
    ));
}

pub(super) fn send_end_of_stream(flv_packet_tx: &mpsc::UnboundedSender<FlvPacket>, live_id: &str) {
    let _ = flv_packet_tx.send(FlvPacket::EndOfStream {
        live_id: live_id.to_string(),
    });
}
