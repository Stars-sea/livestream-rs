use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use tempfile::{Builder, TempDir};

use crate::pipeline::normalize::normalize_component;

pub(super) struct SegmentWorkspace {
    segment_dir: TempDir,
    upload_dir: PathBuf,
}

impl SegmentWorkspace {
    pub(super) fn new(stream_id: &str, cache_dir: &str) -> Result<Self> {
        let cache_root = Self::cache_root(cache_dir);
        fs::create_dir_all(&cache_root)?;

        Ok(Self {
            segment_dir: Self::create_segment_dir(stream_id, &cache_root)?,
            upload_dir: Self::create_upload_dir(stream_id, &cache_root)?,
        })
    }

    fn cache_root(cache_dir: &str) -> PathBuf {
        if cache_dir.trim().is_empty() {
            std::env::temp_dir()
        } else {
            PathBuf::from(cache_dir)
        }
    }

    fn create_segment_dir(stream_id: &str, cache_root: &Path) -> Result<TempDir> {
        let prefix = format!("livestream-rs-segment-{}-", normalize_component(stream_id));
        Ok(Builder::new().prefix(&prefix).tempdir_in(cache_root)?)
    }

    fn create_upload_dir(stream_id: &str, cache_root: &Path) -> Result<PathBuf> {
        let upload_dir = cache_root
            .join("livestream-rs-artifacts")
            .join(normalize_component(stream_id));
        fs::create_dir_all(&upload_dir)?;
        Ok(upload_dir)
    }

    pub(super) fn segment_root(&self) -> &Path {
        self.segment_dir.path()
    }

    pub(super) fn playlist_path(&self) -> PathBuf {
        self.upload_dir.join("index.m3u8")
    }

    pub(super) fn stage_segment(&self, segment_path: &Path) -> Result<PathBuf> {
        let filename = segment_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                anyhow::anyhow!("Invalid segment filename: {}", segment_path.display())
            })?;
        let staged_path = self.upload_dir.join(filename);

        if staged_path.exists() {
            fs::remove_file(&staged_path)?;
        }

        match fs::rename(segment_path, &staged_path) {
            Ok(_) => Ok(staged_path),
            Err(_) => {
                fs::copy(segment_path, &staged_path)?;
                fs::remove_file(segment_path)?;
                Ok(staged_path)
            }
        }
    }
}
