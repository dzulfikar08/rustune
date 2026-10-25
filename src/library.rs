//! Local music library: recursive scan, tag reading (symphonia metadata
//! probing, filename fallback), an on-disk cache keyed by path + mtime + size
//! so restarts are instant, and search over title/artist/album.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTagKey};
use symphonia::core::probe::Hint;

use crate::media::{MediaItem, SourceKind};

/// Bump when the cached entry layout or tag logic changes.
const CACHE_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    mtime_ns: u64,
    size: u64,
    tags: Tags,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    version: u32,
    entries: HashMap<String, CacheEntry>,
}

impl Cache {
    fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Cache>(&bytes).ok())
            .filter(|c| c.version == CACHE_VERSION)
            .unwrap_or_default()
    }
}

fn clean(value: &str) -> Option<String> {
    let trimmed = value.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Fill still-empty fields from one metadata revision.
fn apply_revision(rev: &MetadataRevision, tags: &mut Tags, album_artist: &mut Option<String>) {
    for tag in rev.tags() {
        let slot = match tag.std_key {
            Some(StandardTagKey::TrackTitle) => &mut tags.title,
            Some(StandardTagKey::Artist) => &mut tags.artist,
            Some(StandardTagKey::AlbumArtist) => &mut *album_artist,
            Some(StandardTagKey::Album) => &mut tags.album,
            _ => continue,
        };
        if slot.is_none() {
            *slot = clean(&tag.value.to_string());
        }
    }
}

/// Read title/artist/album/duration from a file's container metadata.
/// Unreadable or unsupported files yield empty tags.
pub fn read_tags(path: &Path) -> Tags {
    let mut tags = Tags::default();
    let Ok(file) = File::open(path) else {
        return tags;
    };
    let mss = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let Ok(mut probed) = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) else {
        return tags;
    };

    // Container metadata (Vorbis comments, MP4 atoms, ...) takes precedence
    // over tags found while probing (e.g. a leading ID3v2 block).
    let mut album_artist = None;
    if let Some(rev) = probed.format.metadata().current() {
        apply_revision(rev, &mut tags, &mut album_artist);
    }
    if let Some(meta) = probed.metadata.get() {
        if let Some(rev) = meta.current() {
            apply_revision(rev, &mut tags, &mut album_artist);
        }
    }
    if tags.artist.is_none() {
        tags.artist = album_artist;
    }

    if let Some(track) = probed.format.default_track() {
        let params = &track.codec_params;
        tags.duration = match (params.n_frames, params.time_base, params.sample_rate) {
            (Some(n), Some(tb), _) => Some(tb.calc_time(n).seconds),
            (Some(n), None, Some(sr)) if sr > 0 => Some(n / u64::from(sr)),
            _ => None,
        };
    }
    tags
}

fn file_stamp(meta: &std::fs::Metadata) -> (u64, u64) {
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    (mtime_ns, meta.len())
}

/// Build a queue/list item for a local file from its tags.
pub fn item_from_tags(path: &Path, tags: Tags) -> MediaItem {
    let stem = || {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned())
    };
    let subtitle = match (&tags.artist, &tags.album) {
        (Some(artist), Some(album)) => Some(format!("{artist} · {album}")),
        (Some(artist), None) => Some(artist.clone()),
        (None, Some(album)) => Some(album.clone()),
        (None, None) => path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned()),
    };
    MediaItem {
        id: path.to_string_lossy().into_owned(),
        title: tags.title.unwrap_or_else(stem),
        artist: tags.artist,
        album: tags.album,
        duration: tags.duration,
        subtitle,
        source: SourceKind::Local,
    }
}

/// Item for a single file, reading its tags (no cache).
pub fn item_for_file(path: &Path) -> MediaItem {
    item_from_tags(path, read_tags(path))
}

pub fn has_audio_extension(path: &Path, extensions: &[String]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)))
}

/// Recursively collect audio files under `root` (hidden entries skipped,
/// symlinked directories not followed, unreadable subdirectories ignored),
/// sorted by path.
pub fn collect_files(root: &Path, extensions: &[String]) -> Result<Vec<PathBuf>> {
    std::fs::read_dir(root)
        .with_context(|| format!("Cannot read music directory {}", root.display()))?;
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if ft.is_dir() {
                stack.push(path);
            } else if (ft.is_file() || (ft.is_symlink() && path.is_file()))
                && has_audio_extension(&path, extensions)
            {
                files.push(path);
            }
        }
    }
    files.sort_by_cached_key(|p| p.to_string_lossy().to_lowercase());
    Ok(files)
}

/// Scan `root`, reusing cached tags for unchanged files. When `cache_path`
/// is given the refreshed cache is written back (only if it changed).
pub fn scan(
    root: &Path,
    extensions: &[String],
    cache_path: Option<&Path>,
) -> Result<Vec<MediaItem>> {
    let files = collect_files(root, extensions)?;
    let mut cache = cache_path.map(Cache::load).unwrap_or_default();
    let mut fresh: HashMap<String, CacheEntry> = HashMap::with_capacity(files.len());
    let mut changed = cache.entries.len() != files.len();
    let mut items = Vec::with_capacity(files.len());

    for path in files {
        let key = path.to_string_lossy().into_owned();
        let (mtime_ns, size) = std::fs::metadata(&path)
            .map(|m| file_stamp(&m))
            .unwrap_or((0, 0));
        let tags = match cache.entries.remove(&key) {
            Some(entry) if entry.mtime_ns == mtime_ns && entry.size == size => entry.tags,
            _ => {
                changed = true;
                read_tags(&path)
            }
        };
        items.push(item_from_tags(&path, tags.clone()));
        fresh.insert(
            key,
            CacheEntry {
                mtime_ns,
                size,
                tags,
            },
        );
    }

    if let Some(cache_path) = cache_path {
        if changed {
            let cache = Cache {
                version: CACHE_VERSION,
                entries: fresh,
            };
            // A failed cache write only costs a slower next start.
            if let Ok(json) = serde_json::to_vec(&cache) {
                let _ = crate::config::write_atomic(cache_path, &json);
            }
        }
    }
    Ok(items)
}

/// Case-insensitive search: every whitespace-separated word must occur in
/// the title, artist or album.
pub fn search(items: &[MediaItem], query: &str) -> Vec<MediaItem> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return items.to_vec();
    }
    items
        .iter()
        .filter(|item| {
            let haystack = format!(
                "{}\n{}\n{}",
                item.title,
                item.artist.as_deref().unwrap_or(""),
                item.album.as_deref().unwrap_or("")
            )
            .to_lowercase();
            words.iter().all(|w| haystack.contains(w.as_str()))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "rustune-lib-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Minimal PCM WAV with a RIFF LIST/INFO chunk carrying title/artist/album.
    fn wav_with_info(seconds: u32, title: &str, artist: &str, album: &str) -> Vec<u8> {
        let rate: u32 = 8000;
        let data_len = rate * seconds; // 8-bit mono
        let mut info = b"INFO".to_vec();
        for (id, val) in [(b"INAM", title), (b"IART", artist), (b"IPRD", album)] {
            let mut v = val.as_bytes().to_vec();
            v.push(0);
            if v.len() % 2 == 1 {
                v.push(0);
            }
            info.extend_from_slice(id);
            info.extend_from_slice(&(v.len() as u32).to_le_bytes());
            info.extend_from_slice(&v);
        }
        let mut body = b"WAVE".to_vec();
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&16u32.to_le_bytes());
        body.extend_from_slice(&1u16.to_le_bytes()); // PCM
        body.extend_from_slice(&1u16.to_le_bytes()); // mono
        body.extend_from_slice(&rate.to_le_bytes());
        body.extend_from_slice(&rate.to_le_bytes()); // byte rate
        body.extend_from_slice(&1u16.to_le_bytes()); // block align
        body.extend_from_slice(&8u16.to_le_bytes()); // bits
        body.extend_from_slice(b"LIST");
        body.extend_from_slice(&(info.len() as u32).to_le_bytes());
        body.extend_from_slice(&info);
        body.extend_from_slice(b"data");
        body.extend_from_slice(&data_len.to_le_bytes());
        body.extend(std::iter::repeat_n(128u8, data_len as usize));
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn exts() -> Vec<String> {
        vec!["wav".into(), "mp3".into()]
    }

    #[test]
    fn reads_wav_info_tags_and_duration() {
        let dir = TempDir::new("tags");
        let p = dir.0.join("x.wav");
        std::fs::write(&p, wav_with_info(2, "Song", "Band", "Record")).unwrap();
        let tags = read_tags(&p);
        assert_eq!(tags.duration, Some(2));
        assert_eq!(tags.title.as_deref(), Some("Song"));
        assert_eq!(tags.artist.as_deref(), Some("Band"));
        assert_eq!(tags.album.as_deref(), Some("Record"));
        let item = item_from_tags(&p, tags);
        assert_eq!(item.subtitle.as_deref(), Some("Band · Record"));
    }

    #[test]
    fn undecodable_file_falls_back_to_filename() {
        let dir = TempDir::new("fallback");
        let album = dir.0.join("Some Album");
        std::fs::create_dir_all(&album).unwrap();
        let p = album.join("07 Track Name.mp3");
        std::fs::write(&p, b"definitely not audio").unwrap();
        let item = item_for_file(&p);
        assert_eq!(item.title, "07 Track Name");
        assert_eq!(item.artist, None);
        assert_eq!(item.subtitle.as_deref(), Some("Some Album"));
        assert_eq!(item.source, SourceKind::Local);
    }

    #[test]
    fn collect_skips_hidden_and_foreign_extensions() {
        let dir = TempDir::new("collect");
        std::fs::create_dir_all(dir.0.join("b/.hidden")).unwrap();
        std::fs::write(dir.0.join("b/2.MP3"), b"x").unwrap();
        std::fs::write(dir.0.join("a.wav"), b"x").unwrap();
        std::fs::write(dir.0.join("notes.txt"), b"x").unwrap();
        std::fs::write(dir.0.join(".secret.mp3"), b"x").unwrap();
        std::fs::write(dir.0.join("b/.hidden/3.mp3"), b"x").unwrap();
        let files = collect_files(&dir.0, &exts()).unwrap();
        let names: Vec<_> = files
            .iter()
            .map(|p| {
                p.strip_prefix(&dir.0)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(names, vec!["a.wav", "b/2.MP3"]);
        assert!(collect_files(&dir.0.join("missing"), &exts()).is_err());
    }

    #[test]
    fn scan_uses_cache_for_unchanged_files() {
        let dir = TempDir::new("cache");
        let music = dir.0.join("music");
        std::fs::create_dir_all(&music).unwrap();
        let song = music.join("s.wav");
        std::fs::write(&song, wav_with_info(1, "Cached", "A", "B")).unwrap();
        let cache = dir.0.join("library.json");

        let first = scan(&music, &exts(), Some(&cache)).unwrap();
        assert_eq!(first[0].title, "Cached");
        assert!(cache.exists());

        // Poison the cached title: an unchanged file must be served from cache.
        let mut c = Cache::load(&cache);
        c.entries
            .values_mut()
            .for_each(|e| e.tags.title = Some("FromCache".into()));
        std::fs::write(&cache, serde_json::to_vec(&c).unwrap()).unwrap();
        let second = scan(&music, &exts(), Some(&cache)).unwrap();
        assert_eq!(second[0].title, "FromCache");

        // A changed file (different size) is re-read.
        std::fs::write(&song, wav_with_info(2, "Fresh", "A", "B")).unwrap();
        let third = scan(&music, &exts(), Some(&cache)).unwrap();
        assert_eq!(third[0].title, "Fresh");
        assert_eq!(third[0].duration, Some(2));
    }

    #[test]
    fn search_matches_all_words_across_fields() {
        let mk = |title: &str, artist: Option<&str>, album: Option<&str>| MediaItem {
            id: title.into(),
            title: title.into(),
            artist: artist.map(String::from),
            album: album.map(String::from),
            duration: None,
            subtitle: None,
            source: SourceKind::Local,
        };
        let items = vec![
            mk("Paranoid Android", Some("Radiohead"), Some("OK Computer")),
            mk("Karma Police", Some("Radiohead"), Some("OK Computer")),
            mk("Android Lust", None, None),
        ];
        let hits =
            |q: &str| -> Vec<String> { search(&items, q).into_iter().map(|i| i.title).collect() };
        assert_eq!(hits("android"), vec!["Paranoid Android", "Android Lust"]);
        assert_eq!(hits("RADIOHEAD karma"), vec!["Karma Police"]);
        assert_eq!(hits("computer"), vec!["Paranoid Android", "Karma Police"]);
        assert_eq!(hits("  ").len(), 3);
        assert!(hits("radiohead lust").is_empty());
    }
}
