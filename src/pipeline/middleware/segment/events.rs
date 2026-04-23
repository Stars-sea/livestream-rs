use std::path::PathBuf;

use crate::dispatcher::{self, SessionEvent};

pub(super) struct SegmentCompleteCommand {
    path: PathBuf,
}

impl SegmentCompleteCommand {
    pub(super) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn dispatch(self, live_id: &str) {
        dispatcher::INSTANCE.send(SessionEvent::SegmentComplete {
            live_id: live_id.to_string(),
            path: self.path,
        });
    }
}

pub(super) struct PlaylistUpdatedCommand {
    path: PathBuf,
    is_final: bool,
}

impl PlaylistUpdatedCommand {
    pub(super) fn new(path: PathBuf, is_final: bool) -> Self {
        Self { path, is_final }
    }

    fn dispatch(self, live_id: &str) {
        dispatcher::INSTANCE.send(SessionEvent::PlaylistUpdated {
            live_id: live_id.to_string(),
            path: self.path,
            is_final: self.is_final,
        });
    }
}

pub(super) struct EventDispatchPlan {
    segment_complete: Option<SegmentCompleteCommand>,
    playlist_updated: Option<PlaylistUpdatedCommand>,
}

impl EventDispatchPlan {
    pub(super) fn completed_segment(
        segment_path: PathBuf,
        playlist_path: PathBuf,
        is_final: bool,
    ) -> Self {
        Self {
            segment_complete: Some(SegmentCompleteCommand::new(segment_path)),
            playlist_updated: Some(PlaylistUpdatedCommand::new(playlist_path, is_final)),
        }
    }

    pub(super) fn playlist_only(playlist_path: PathBuf, is_final: bool) -> Self {
        Self {
            segment_complete: None,
            playlist_updated: Some(PlaylistUpdatedCommand::new(playlist_path, is_final)),
        }
    }

    pub(super) fn dispatch(self, live_id: &str) {
        if let Some(segment_complete) = self.segment_complete {
            segment_complete.dispatch(live_id);
        }

        if let Some(playlist_updated) = self.playlist_updated {
            playlist_updated.dispatch(live_id);
        }
    }
}
