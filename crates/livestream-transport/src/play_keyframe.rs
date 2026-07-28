//! Shared keyframe-recovery logic for FLV playback (RTMP + HTTP-FLV).
//!
//! After a broadcast receiver lags (tokio `RecvError::Lagged`), the player
//! skips non-keyframe video tags until the next keyframe arrives, avoiding
//! visual corruption from incomplete frame data.

use livestream_media::flv::FlvTag;

/// Check whether the given FLV tag should be skipped while waiting for a
/// keyframe after receiver lag.
///
/// Returns `true` if the tag should be skipped; resets `waiting_keyframe`
/// to `false` when a video keyframe is found.
pub fn should_skip_while_waiting_keyframe(waiting_keyframe: &mut bool, tag: &FlvTag) -> bool {
    if !*waiting_keyframe {
        return false;
    }

    match tag {
        FlvTag::Video {
            is_keyframe: true, ..
        } => {
            *waiting_keyframe = false;
            false
        }
        FlvTag::Video { .. } => true,
        _ => false,
    }
}
