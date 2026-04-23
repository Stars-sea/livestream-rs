use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;

pub(super) struct PlaylistEntry {
    pub(super) filename: String,
    pub(super) duration_secs: f64,
}

pub(super) struct PlaylistState {
    path: PathBuf,
    target_duration: u64,
    entries: Vec<PlaylistEntry>,
    next_snapshot_id: u64,
}

impl PlaylistState {
    pub(super) fn new(path: PathBuf, segment_duration: Duration) -> Self {
        let target_duration = segment_duration.as_secs_f64().ceil().max(1.0) as u64;
        Self {
            path,
            target_duration,
            entries: Vec::new(),
            next_snapshot_id: 0,
        }
    }

    pub(super) fn push_segment(&mut self, filename: String, duration: Duration) {
        self.entries.push(PlaylistEntry {
            filename,
            duration_secs: duration.as_secs_f64().max(0.001),
        });
    }

    pub(super) fn has_entries(&self) -> bool {
        !self.entries.is_empty()
    }

    pub(super) fn write_snapshot(&mut self, is_final: bool) -> Result<PathBuf> {
        let path = if is_final {
            self.path.clone()
        } else {
            let snapshot_path = self
                .path
                .with_file_name(format!("index-{:04}.m3u8", self.next_snapshot_id));
            self.next_snapshot_id = self.next_snapshot_id.saturating_add(1);
            snapshot_path
        };

        fs::write(&path, self.render(is_final))?;
        Ok(path)
    }

    pub(super) fn render(&self, is_final: bool) -> String {
        let target_duration = self
            .entries
            .iter()
            .map(|entry| entry.duration_secs.ceil() as u64)
            .max()
            .unwrap_or(self.target_duration)
            .max(self.target_duration);

        let mut lines = vec![
            "#EXTM3U".to_string(),
            "#EXT-X-VERSION:3".to_string(),
            format!("#EXT-X-TARGETDURATION:{target_duration}"),
            "#EXT-X-MEDIA-SEQUENCE:0".to_string(),
        ];

        for entry in &self.entries {
            lines.push(format!("#EXTINF:{:.3},", entry.duration_secs));
            lines.push(entry.filename.clone());
        }

        if is_final {
            lines.push("#EXT-X-ENDLIST".to_string());
        }

        lines.push(String::new());
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::{PlaylistEntry, PlaylistState};
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn renders_non_final_playlist_without_endlist() {
        let mut playlist = PlaylistState::new(PathBuf::from("index.m3u8"), Duration::from_secs(5));
        playlist.entries.push(PlaylistEntry {
            filename: "segment_0000.ts".to_string(),
            duration_secs: 4.2,
        });

        let rendered = playlist.render(false);

        assert!(rendered.contains("#EXTM3U"));
        assert!(rendered.contains("#EXTINF:4.200,"));
        assert!(rendered.contains("segment_0000.ts"));
        assert!(!rendered.contains("#EXT-X-ENDLIST"));
    }

    #[test]
    fn renders_final_playlist_with_endlist() {
        let mut playlist = PlaylistState::new(PathBuf::from("index.m3u8"), Duration::from_secs(5));
        playlist.entries.push(PlaylistEntry {
            filename: "segment_0000.ts".to_string(),
            duration_secs: 6.1,
        });

        let rendered = playlist.render(true);

        assert!(rendered.contains("#EXT-X-TARGETDURATION:7"));
        assert!(rendered.contains("#EXT-X-ENDLIST"));
    }
}
