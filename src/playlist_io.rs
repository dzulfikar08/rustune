//! M3U/M3U8/PLS playlist reading and M3U writing.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::extractor;
use crate::media::{MediaItem, SourceKind};

/// One playlist line, before it is turned into a `MediaItem`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistEntry {
    /// Absolute local path or URL.
    pub location: String,
    pub title: Option<String>,
    pub duration: Option<u64>,
}

pub fn is_playlist_path(path: &Path) -> bool {
    matches!(
        extension_lower(path).as_deref(),
        Some("m3u" | "m3u8" | "pls")
    )
}

fn extension_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

/// Decode playlist bytes: strip a UTF-8 BOM, replace invalid UTF-8.
fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

fn is_url(s: &str) -> bool {
    s.split_once("://").is_some_and(|(scheme, _)| {
        !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

/// Minimal `%XX` decoding for `file://` URLs.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve a playlist location against the playlist's directory.
fn resolve_location(raw: &str, base: Option<&Path>) -> String {
    let raw = raw.trim();
    if let Some(path) = raw.strip_prefix("file://") {
        // file:///abs/path or file://localhost/abs/path
        let path = path.strip_prefix("localhost").unwrap_or(path);
        return percent_decode(path);
    }
    if is_url(raw) {
        return raw.to_string();
    }
    #[cfg(not(windows))]
    let owned = raw.replace('\\', "/");
    #[cfg(not(windows))]
    let raw = owned.as_str();
    let path = Path::new(raw);
    if path.is_absolute() {
        return raw.to_string();
    }
    match base {
        Some(dir) => dir.join(path).to_string_lossy().into_owned(),
        None => raw.to_string(),
    }
}

/// Parse M3U / extended M3U text. `base` is the directory relative entries
/// are resolved against.
pub fn parse_m3u(text: &str, base: Option<&Path>) -> Vec<PlaylistEntry> {
    let mut entries = Vec::new();
    let mut pending: Option<(Option<u64>, Option<String>)> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(info) = line.strip_prefix("#EXTINF:") {
            let (dur, title) = match info.split_once(',') {
                Some((d, t)) => (d, Some(t.trim())),
                None => (info, None),
            };
            // Duration may carry attributes: `#EXTINF:123 tvg-id="x",Title`.
            let dur = dur
                .split_whitespace()
                .next()
                .and_then(|d| d.parse::<f64>().ok())
                .filter(|d| *d >= 0.0)
                .map(|d| d.round() as u64);
            let title = title.filter(|t| !t.is_empty()).map(String::from);
            pending = Some((dur, title));
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let (duration, title) = pending.take().unwrap_or((None, None));
        entries.push(PlaylistEntry {
            location: resolve_location(line, base),
            title,
            duration,
        });
    }
    entries
}

/// Parse a PLS (`[playlist]`, `FileN=`, `TitleN=`, `LengthN=`).
pub fn parse_pls(text: &str, base: Option<&Path>) -> Vec<PlaylistEntry> {
    #[derive(Default)]
    struct Raw {
        file: Option<String>,
        title: Option<String>,
        length: Option<u64>,
    }
    let mut by_index: BTreeMap<u32, Raw> = BTreeMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        let (field, num) = if let Some(n) = key.strip_prefix("file") {
            ("file", n)
        } else if let Some(n) = key.strip_prefix("title") {
            ("title", n)
        } else if let Some(n) = key.strip_prefix("length") {
            ("length", n)
        } else {
            continue;
        };
        let Ok(idx) = num.parse::<u32>() else {
            continue;
        };
        let raw = by_index.entry(idx).or_default();
        match field {
            "file" => raw.file = Some(value.to_string()),
            "title" if !value.is_empty() => raw.title = Some(value.to_string()),
            "length" => {
                raw.length = value
                    .parse::<i64>()
                    .ok()
                    .and_then(|l| u64::try_from(l).ok())
            }
            _ => {}
        }
    }
    by_index
        .into_values()
        .filter_map(|r| {
            Some(PlaylistEntry {
                location: resolve_location(&r.file?, base),
                title: r.title,
                duration: r.length,
            })
        })
        .collect()
}

/// Turn a playlist entry into a queue item: YouTube URLs go through the
/// extractor, other URLs stream directly, everything else is a local file.
pub fn entry_to_item(entry: PlaylistEntry) -> MediaItem {
    if let Some((name, id)) = extractor::parse_url(&entry.location) {
        return MediaItem {
            title: entry.title.unwrap_or_else(|| entry.location.clone()),
            id,
            artist: None,
            album: None,
            duration: entry.duration,
            subtitle: None,
            source: SourceKind::Extractor(name.to_string()),
        };
    }
    if is_url(&entry.location) {
        return MediaItem {
            title: entry.title.unwrap_or_else(|| entry.location.clone()),
            id: entry.location,
            artist: None,
            album: None,
            duration: entry.duration,
            subtitle: None,
            source: SourceKind::Url,
        };
    }
    let path = Path::new(&entry.location);
    let title = entry.title.unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| entry.location.clone())
    });
    let subtitle = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned());
    MediaItem {
        id: entry.location,
        title,
        artist: None,
        album: None,
        duration: entry.duration,
        subtitle,
        source: SourceKind::Local,
    }
}

/// Read an M3U/M3U8/PLS file into queue items.
pub fn read_playlist(path: &Path) -> Result<Vec<MediaItem>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("Cannot read playlist {}", path.display()))?;
    let text = decode(&bytes);
    let base = path.parent();
    let entries = if extension_lower(path).as_deref() == Some("pls")
        || text
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("[playlist]")
    {
        parse_pls(&text, base)
    } else {
        parse_m3u(&text, base)
    };
    Ok(entries.into_iter().map(entry_to_item).collect())
}

/// Location written for an item (absolute path, URL, or extractor URL).
fn item_location(item: &MediaItem) -> String {
    match &item.source {
        SourceKind::Local | SourceKind::Url => item.id.clone(),
        SourceKind::Extractor(name) => {
            extractor::item_url(name, &item.id).unwrap_or_else(|| item.id.clone())
        }
    }
}

/// Render items as extended M3U (UTF-8, LF line endings).
pub fn write_m3u(items: &[MediaItem]) -> String {
    let mut out = String::from("#EXTM3U\n");
    for item in items {
        let dur = item.duration.map_or(-1, |d| d as i64);
        let title = match &item.artist {
            Some(artist) => format!("{artist} - {}", item.title),
            None => item.title.clone(),
        };
        // Titles are single-line by construction; guard against stray newlines.
        let title = title.replace(['\r', '\n'], " ");
        let _ = writeln!(out, "#EXTINF:{dur},{title}");
        let _ = writeln!(out, "{}", item_location(item));
    }
    out
}

/// Turn a user-typed name into a safe playlist file stem.
pub fn sanitize_name(name: &str) -> Option<String> {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '(' | ')') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace());
    let cleaned: String = cleaned.chars().take(100).collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Save `items` as `<dir>/<name>.m3u`; returns the written path.
pub fn save_named(dir: &Path, name: &str, items: &[MediaItem]) -> Result<PathBuf> {
    let stem = sanitize_name(name).context("Playlist name is empty")?;
    let stem = stem
        .strip_suffix(".m3u")
        .or_else(|| stem.strip_suffix(".m3u8"))
        .unwrap_or(&stem);
    let path = dir.join(format!("{stem}.m3u"));
    crate::config::write_atomic(&path, write_m3u(items).as_bytes())
        .with_context(|| format!("Cannot write {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(path: &str, title: &str, artist: Option<&str>, dur: Option<u64>) -> MediaItem {
        MediaItem {
            id: path.into(),
            title: title.into(),
            artist: artist.map(String::from),
            album: None,
            duration: dur,
            subtitle: None,
            source: SourceKind::Local,
        }
    }

    #[test]
    fn m3u_extinf_relative_crlf_and_bom() {
        let bytes = b"\xEF\xBB\xBF#EXTM3U\r\n#EXTINF:215,Artist - Song\r\nsub/song.mp3\r\n\r\n# comment\r\n/abs/other.flac\r\n";
        let text = decode(bytes);
        let e = parse_m3u(&text, Some(Path::new("/music/lists")));
        assert_eq!(e.len(), 2);
        let joined = Path::new("/music/lists").join("sub/song.mp3");
        assert_eq!(e[0].location, joined.to_string_lossy());
        assert_eq!(e[0].title.as_deref(), Some("Artist - Song"));
        assert_eq!(e[0].duration, Some(215));
        assert_eq!(e[1].location, "/abs/other.flac");
        assert_eq!(e[1].title, None);
        assert_eq!(e[1].duration, None);
    }

    #[test]
    fn m3u_negative_duration_attributes_and_urls() {
        let text = "#EXTM3U\n#EXTINF:-1 tvg-id=\"x\",Radio One\nhttp://radio.example/stream\n#EXTINF:12.6,\nfile:///tmp/a%20b.mp3\n";
        let e = parse_m3u(text, None);
        assert_eq!(e[0].location, "http://radio.example/stream");
        assert_eq!(e[0].duration, None);
        assert_eq!(e[0].title.as_deref(), Some("Radio One"));
        assert_eq!(e[1].location, "/tmp/a b.mp3");
        assert_eq!(e[1].duration, Some(13));
        assert_eq!(e[1].title, None);
    }

    #[test]
    fn m3u_extinf_without_following_entry_is_dropped() {
        assert!(parse_m3u("#EXTM3U\n#EXTINF:10,Ghost\n", None).is_empty());
        assert!(parse_m3u("", None).is_empty());
    }

    #[cfg(not(windows))]
    #[test]
    fn m3u_windows_separators_become_slashes() {
        let e = parse_m3u("Artist\\Album\\01.mp3\n", Some(Path::new("/m")));
        assert_eq!(e[0].location, "/m/Artist/Album/01.mp3");
    }

    #[test]
    fn pls_parses_out_of_order_and_case_insensitive_keys() {
        let text = "[playlist]\r\nNumberOfEntries=2\r\nFile2=http://s.example/2\r\nTitle2=Two\r\nfile1=one.ogg\r\nLength1=61\r\nLength2=-1\r\nVersion=2\r\n";
        let e = parse_pls(text, Some(Path::new("/p")));
        assert_eq!(e.len(), 2);
        assert_eq!(
            e[0].location,
            Path::new("/p").join("one.ogg").to_string_lossy()
        );
        assert_eq!(e[0].duration, Some(61));
        assert_eq!(e[0].title, None);
        assert_eq!(e[1].location, "http://s.example/2");
        assert_eq!(e[1].title.as_deref(), Some("Two"));
        assert_eq!(e[1].duration, None);
    }

    #[test]
    fn pls_skips_titles_without_files() {
        let e = parse_pls("[playlist]\nTitle3=orphan\nFile1=/a.mp3\n", None);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].location, "/a.mp3");
    }

    #[test]
    fn entries_map_to_sources() {
        let yt = entry_to_item(PlaylistEntry {
            location: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".into(),
            title: Some("Never".into()),
            duration: Some(212),
        });
        assert_eq!(yt.source, SourceKind::Extractor("ytdlp".into()));
        assert_eq!(yt.id, "dQw4w9WgXcQ");
        let url = entry_to_item(PlaylistEntry {
            location: "https://radio.example/live.mp3".into(),
            title: None,
            duration: None,
        });
        assert_eq!(url.source, SourceKind::Url);
        assert_eq!(url.title, "https://radio.example/live.mp3");
        let file = entry_to_item(PlaylistEntry {
            location: "/m/Album/01 Intro.flac".into(),
            title: None,
            duration: None,
        });
        assert_eq!(file.source, SourceKind::Local);
        assert_eq!(file.title, "01 Intro");
        assert_eq!(file.subtitle.as_deref(), Some("Album"));
    }

    #[test]
    fn write_then_read_round_trip() {
        let items = vec![
            local("/m/a.mp3", "Song A", Some("Band"), Some(100)),
            local("/m/b c.flac", "B\nC", None, None),
            MediaItem {
                id: "abc123".into(),
                title: "Video".into(),
                artist: None,
                album: None,
                duration: Some(5),
                subtitle: None,
                source: SourceKind::Extractor("ytdlp".into()),
            },
        ];
        let text = write_m3u(&items);
        assert!(text.starts_with("#EXTM3U\n#EXTINF:100,Band - Song A\n/m/a.mp3\n"));
        assert!(text.contains("#EXTINF:-1,B C\n/m/b c.flac\n"));
        let back: Vec<MediaItem> = parse_m3u(&text, None)
            .into_iter()
            .map(entry_to_item)
            .collect();
        assert_eq!(back.len(), 3);
        assert_eq!(back[0].id, "/m/a.mp3");
        assert_eq!(back[0].title, "Band - Song A");
        assert_eq!(back[0].duration, Some(100));
        assert_eq!(back[1].id, "/m/b c.flac");
        assert_eq!(back[2].source, SourceKind::Extractor("ytdlp".into()));
        assert_eq!(back[2].id, "abc123");
    }

    #[test]
    fn read_playlist_from_disk_resolves_relative_to_file() {
        let dir = std::env::temp_dir().join(format!("rustune-pl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let m3u = dir.join("list.M3U8");
        std::fs::write(&m3u, "song.mp3\n").unwrap();
        let items = read_playlist(&m3u).unwrap();
        assert_eq!(items[0].id, dir.join("song.mp3").to_string_lossy());
        let pls = dir.join("radio.pls");
        std::fs::write(&pls, "[playlist]\nFile1=http://x.example/s\n").unwrap();
        let items = read_playlist(&pls).unwrap();
        assert_eq!(items[0].source, SourceKind::Url);
        let saved = save_named(&dir, "  My/Mix?.m3u ", &items).unwrap();
        assert_eq!(saved, dir.join("My_Mix_.m3u"));
        assert_eq!(read_playlist(&saved).unwrap()[0].id, "http://x.example/s");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn playlist_detection_and_name_sanitizing() {
        assert!(is_playlist_path(Path::new("a/b.M3U")));
        assert!(is_playlist_path(Path::new("x.pls")));
        assert!(!is_playlist_path(Path::new("x.mp3")));
        assert_eq!(
            sanitize_name("../../etc/passwd").as_deref(),
            Some("_.._etc_passwd")
        );
        assert_eq!(sanitize_name("   "), None);
        assert_eq!(sanitize_name("..."), None);
        assert_eq!(
            sanitize_name("Road Trip (2024)").as_deref(),
            Some("Road Trip (2024)")
        );
    }
}
