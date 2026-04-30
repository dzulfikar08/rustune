use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::media::{MediaItem, SourceKind};

pub struct LocalSource {
    pub music_dir: PathBuf,
    pub extensions: Vec<String>,
}

impl LocalSource {
    pub fn new(music_dir: PathBuf, extensions: Vec<String>) -> Self {
        Self {
            music_dir,
            extensions,
        }
    }

    /// Synchronous recursive scan of the music directory.
    pub fn scan_sync(&self) -> Result<Vec<MediaItem>> {
        if !self.music_dir.exists() {
            anyhow::bail!("Music directory does not exist: {}", self.music_dir.display());
        }

        let mut items = Vec::new();
        self.scan_dir(&self.music_dir, &mut items)?;
        items.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        Ok(items)
    }

    /// Filter a pre-scanned list by a search query (case-insensitive substring match
    /// on title and subtitle).
    pub fn search(items: &[MediaItem], query: &str) -> Vec<MediaItem> {
        let q = query.to_lowercase();
        items
            .iter()
            .filter(|item| {
                item.title.to_lowercase().contains(&q)
                    || item
                        .subtitle
                        .as_ref()
                        .map_or(false, |s| s.to_lowercase().contains(&q))
            })
            .cloned()
            .collect()
    }

    fn scan_dir(&self, dir: &std::path::Path, items: &mut Vec<MediaItem>) -> Result<()> {
        let entries = std::fs::read_dir(dir).context(format!("Failed to read dir: {}", dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                self.scan_dir(&path, items)?;
                continue;
            }

            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if self.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
                    let title = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Unknown")
                        .to_string();

                    items.push(MediaItem {
                        id: path.to_string_lossy().to_string(),
                        title,
                        duration: None,
                        subtitle: path
                            .parent()
                            .and_then(|p| p.file_name())
                            .and_then(|n| n.to_str())
                            .map(|s| s.to_string()),
                        source: SourceKind::Local,
                    });
                }
            }
        }

        Ok(())
    }
}
