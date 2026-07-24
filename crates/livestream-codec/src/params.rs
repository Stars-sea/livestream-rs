use serde::Deserialize;

/// Configuration for HLS segment production.
///
/// Used by HlsSegmenter (Phase 4) to control segment duration, storage paths,
/// playlist size, and upload staging.
#[derive(Clone, Debug, Deserialize)]
pub struct SegmentConfig {
    /// Target duration of each TS segment in seconds.
    pub duration_secs: u64,

    /// Directory for temporary segment files before upload.
    pub cache_dir: String,

    /// Maximum number of segments to keep in the playlist (0 = unlimited).
    pub playlist_size: usize,

    /// Object key prefix in MinIO (e.g., "hls/{live_id}/").
    pub minio_prefix: String,

    /// Maximum number of staged-but-not-yet-uploaded segment files.
    /// When exceeded, the oldest staged file is evicted (LRU).
    /// Prevents disk exhaustion when MinIO is unreachable.
    pub max_staged_segments: usize,
}
