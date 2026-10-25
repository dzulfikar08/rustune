//! Lyrics from LRCLIB (<https://lrclib.net>) and an LRC parser.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use ureq::Agent;

use super::{get_bytes, non_empty, parse_json, push_query, ProviderError, Result};

pub const LRCLIB_URL: &str = "https://lrclib.net";

const MAX_BODY: u64 = 2 << 20;
/// LRCLIB's own matching tolerance for `/api/get` is ±2 s.
const DURATION_TOLERANCE: f64 = 3.0;

/// Lyrics settings for the application config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LyricsConfig {
    pub enabled: bool,
    /// LRCLIB-compatible server.
    pub server: String,
}

impl Default for LyricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            server: LRCLIB_URL.to_string(),
        }
    }
}

/// Lyrics of one track.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lyrics {
    /// Timed lines sorted by start time (stable for equal times).
    pub synced: Vec<(Duration, String)>,
    /// Untimed text, when available.
    pub plain: Option<String>,
    /// The database marks the track as instrumental.
    pub instrumental: bool,
}

impl Lyrics {
    /// Interprets `text` as LRC; text without timestamps becomes `plain`.
    pub fn from_text(text: &str) -> Self {
        let synced = parse_lrc(text);
        let plain = if synced.is_empty() {
            let t = text.trim();
            (!t.is_empty()).then(|| t.to_string())
        } else {
            None
        };
        Self {
            synced,
            plain,
            instrumental: false,
        }
    }

    pub fn is_synced(&self) -> bool {
        !self.synced.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.synced.is_empty() && self.plain.is_none()
    }

    /// Index of the line being sung at `pos`: the last line starting at or
    /// before `pos`. `None` before the first line.
    pub fn line_index_at(&self, pos: Duration) -> Option<usize> {
        self.synced
            .partition_point(|(start, _)| *start <= pos)
            .checked_sub(1)
    }
}

/// Parses LRC text.
///
/// - `[mm:ss]`, `[mm:ss.x]`, `[mm:ss.xx]`, `[mm:ss.xxx]` and `[mm:ss:xx]`
///   timestamps; minutes may exceed two digits;
/// - several timestamps on one line repeat the line at each time;
/// - `[offset:±ms]` shifts every line (positive = earlier), clamped at 0;
/// - metadata tags (`[ar:…]`, `[ti:…]`, …) and untimed lines are ignored;
/// - enhanced-LRC word stamps (`<mm:ss.xx>`) are stripped from the text.
pub fn parse_lrc(text: &str) -> Vec<(Duration, String)> {
    let mut offset_ms: i64 = 0;
    let mut lines: Vec<(i64, String)> = Vec::new();
    let mut stamps: Vec<i64> = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim_start_matches('\u{feff}').trim();
        stamps.clear();
        while let Some(after) = rest.strip_prefix('[') {
            let Some(end) = after.find(']') else { break };
            let tag = &after[..end];
            if let Some(ms) = parse_timestamp(tag) {
                stamps.push(ms);
            } else if let Some((key, value)) = tag.split_once(':') {
                if key.trim().eq_ignore_ascii_case("offset") {
                    if let Ok(v) = value.trim().parse::<i64>() {
                        offset_ms = v;
                    }
                }
            }
            rest = after[end + 1..].trim_start();
        }
        let Some((&last, others)) = stamps.split_last() else {
            continue;
        };
        let line = strip_word_stamps(rest.trim_end());
        for &ms in others {
            lines.push((ms, line.clone()));
        }
        lines.push((last, line));
    }
    lines.sort_by_key(|(ms, _)| *ms);
    lines
        .into_iter()
        .map(|(ms, line)| {
            let shifted = ms.saturating_sub(offset_ms).max(0);
            (Duration::from_millis(shifted as u64), line)
        })
        .collect()
}

/// `mm:ss`, `mm:ss.f{1,3}` or `mm:ss:ff` → milliseconds.
fn parse_timestamp(tag: &str) -> Option<i64> {
    let tag = tag.trim();
    let (min, rest) = tag.split_once(':')?;
    let (sec, frac) = match rest.find(['.', ':']) {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(min) || !digits(sec) || sec.len() > 2 || (!frac.is_empty() && !digits(frac)) {
        return None;
    }
    let min: i64 = min.parse().ok()?;
    let sec: i64 = sec.parse().ok()?;
    if sec >= 60 {
        return None;
    }
    let frac_ms = match frac.len() {
        0 => 0,
        1 => frac.parse::<i64>().ok()? * 100,
        2 => frac.parse::<i64>().ok()? * 10,
        _ => frac[..3].parse::<i64>().ok()?,
    };
    min.checked_mul(60_000)?.checked_add(sec * 1000 + frac_ms)
}

fn strip_word_stamps(text: &str) -> String {
    if !text.contains('<') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('>') {
            Some(end) if parse_timestamp(&after[..end]).is_some() => rest = &after[end + 1..],
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    let collapsed: Vec<&str> = out.split_whitespace().collect();
    collapsed.join(" ")
}

/// Strips video-upload noise from a title for searching: `[...]` blocks and
/// parenthesised `official`/`lyric`/`video`/`audio`/`visualizer` labels.
pub fn clean_title(title: &str) -> String {
    const NOISE: &[&str] = &[
        "official",
        "lyric",
        "video",
        "audio",
        "visualizer",
        "hd",
        "4k",
    ];
    let mut out = String::with_capacity(title.len());
    let mut rest = title;
    loop {
        let next = rest.find(['[', '(']);
        let Some(start) = next else {
            out.push_str(rest);
            break;
        };
        let close = if rest.as_bytes()[start] == b'[' {
            ']'
        } else {
            ')'
        };
        let Some(len) = rest[start..].find(close) else {
            out.push_str(rest);
            break;
        };
        let inner = &rest[start + 1..start + len];
        let lower = inner.to_ascii_lowercase();
        let noisy = close == ']'
            || lower
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|w| NOISE.contains(&w));
        out.push_str(&rest[..start]);
        if !noisy {
            out.push_str(&rest[start..=start + len]);
        }
        rest = &rest[start + len + 1..];
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What to look up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackQuery<'a> {
    pub artist: &'a str,
    pub title: &'a str,
    pub album: Option<&'a str>,
    /// Track length in seconds.
    pub duration: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    duration: Option<f64>,
    instrumental: Option<bool>,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
}

impl Record {
    fn into_lyrics(self) -> Option<Lyrics> {
        let synced = non_empty(self.synced_lyrics)
            .map(|s| parse_lrc(&s))
            .unwrap_or_default();
        let lyrics = Lyrics {
            synced,
            plain: non_empty(self.plain_lyrics),
            instrumental: self.instrumental.unwrap_or(false),
        };
        (!lyrics.is_empty() || lyrics.instrumental).then_some(lyrics)
    }

    fn has_synced(&self) -> bool {
        self.synced_lyrics
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
    }

    fn has_plain(&self) -> bool {
        self.plain_lyrics
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
    }

    fn duration_matches(&self, wanted: Option<u64>) -> bool {
        match (wanted, self.duration) {
            (Some(w), Some(d)) => (d - w as f64).abs() <= DURATION_TOLERANCE,
            _ => true,
        }
    }
}

/// LRCLIB client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LrclibClient {
    base: String,
}

impl Default for LrclibClient {
    fn default() -> Self {
        Self::new(LRCLIB_URL)
    }
}

impl LrclibClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base: super::normalize_base(base_url),
        }
    }

    /// Exact match via `/api/get` (when album and duration are known), else
    /// the best `/api/search` hit. `Ok(None)` when nothing was found.
    pub fn fetch(&self, agent: &Agent, query: &TrackQuery<'_>) -> Result<Option<Lyrics>> {
        if query.title.trim().is_empty() {
            return Ok(None);
        }
        if query.album.is_some() && query.duration.is_some() {
            match self.get(agent, query) {
                Ok(Some(lyrics)) => return Ok(Some(lyrics)),
                // 400: the server rejected the parameters; searching may still work.
                Ok(None) | Err(ProviderError::Status(400)) => {}
                Err(e) => return Err(e),
            }
        }
        self.search(agent, query)
    }

    /// `/api/get`: exact signature match. 404 → `Ok(None)`.
    pub fn get(&self, agent: &Agent, query: &TrackQuery<'_>) -> Result<Option<Lyrics>> {
        let mut url = format!("{}/api/get", self.base);
        let duration = query.duration.map(|d| d.to_string());
        push_query(
            &mut url,
            [("artist_name", query.artist), ("track_name", query.title)],
        );
        if let Some(album) = query.album {
            push_query(&mut url, [("album_name", album)]);
        }
        if let Some(duration) = duration.as_deref() {
            push_query(&mut url, [("duration", duration)]);
        }
        match get_bytes(agent, &url, "application/json", MAX_BODY) {
            Ok(bytes) => Ok(parse_json::<Record>(&bytes)?.into_lyrics()),
            Err(ProviderError::Status(404)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `/api/search` with a cleaned title; picks the best record.
    pub fn search(&self, agent: &Agent, query: &TrackQuery<'_>) -> Result<Option<Lyrics>> {
        let title = clean_title(query.title);
        let title = if title.is_empty() {
            query.title.trim()
        } else {
            title.as_str()
        };
        let artist = query.artist.trim();
        let mut url = format!("{}/api/search", self.base);
        if artist.is_empty() {
            push_query(&mut url, [("q", title)]);
        } else {
            push_query(&mut url, [("track_name", title), ("artist_name", artist)]);
        }
        let bytes = get_bytes(agent, &url, "application/json", MAX_BODY)?;
        let records: Vec<Record> = parse_json(&bytes)?;
        Ok(best_record(records, query.duration).and_then(Record::into_lyrics))
    }
}

/// Preference: synced with matching length, any synced, plain with
/// matching length, any plain.
fn best_record(records: Vec<Record>, duration: Option<u64>) -> Option<Record> {
    let rank = |r: &Record| match (r.has_synced(), r.has_plain(), r.duration_matches(duration)) {
        (true, _, true) => 0,
        (true, _, false) => 1,
        (false, true, true) => 2,
        (false, true, false) => 3,
        (false, false, _) => 4,
    };
    let best = records
        .into_iter()
        .enumerate()
        .min_by_key(|(i, r)| (rank(r), *i))?
        .1;
    (rank(&best) < 4).then_some(best)
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{agent, request_line, MockServer, Reply};
    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    #[test]
    fn parses_lrc_variants() {
        let text = "\u{feff}[ti:Song]\n[ar:Someone]\n[length: 03:20]\n\
                    [00:12.00]First line\n\
                    [00:10.5][01:00:25] Chorus  \n\
                    [00:15.123]<00:15.123>Word <00:16.00>stamps\n\
                    untimed garbage\n\
                    [100:01.00]Long song\n\
                    [00:20.00]\n\
                    [00:61.00]bad seconds\n";
        let lines = parse_lrc(text);
        assert_eq!(
            lines,
            vec![
                (ms(10_500), "Chorus".to_string()),
                (ms(12_000), "First line".to_string()),
                (ms(15_123), "Word stamps".to_string()),
                (ms(20_000), String::new()),
                (ms(60_250), "Chorus".to_string()),
                (ms(6_001_000), "Long song".to_string()),
            ]
        );
    }

    #[test]
    fn applies_offset_and_clamps() {
        let lines = parse_lrc("[offset:+500]\n[00:00.20]a\n[00:02.00]b");
        assert_eq!(lines, vec![(ms(0), "a".into()), (ms(1_500), "b".into())]);
        let lines = parse_lrc("[00:01.00]a\n[offset:-250]");
        assert_eq!(lines, vec![(ms(1_250), "a".into())]);
    }

    #[test]
    fn equal_times_keep_file_order() {
        let lines = parse_lrc("[00:05.00]one\n[00:05.00]two\n[00:01]zero");
        let texts: Vec<&str> = lines.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(texts, ["zero", "one", "two"]);
    }

    #[test]
    fn line_index_binary_search() {
        let lyrics = Lyrics::from_text("[00:01.00]a\n[00:03.00]b\n[00:05.00]c");
        assert_eq!(lyrics.line_index_at(ms(0)), None);
        assert_eq!(lyrics.line_index_at(ms(999)), None);
        assert_eq!(lyrics.line_index_at(ms(1_000)), Some(0));
        assert_eq!(lyrics.line_index_at(ms(2_999)), Some(0));
        assert_eq!(lyrics.line_index_at(ms(3_000)), Some(1));
        assert_eq!(lyrics.line_index_at(ms(99_000)), Some(2));
        assert_eq!(Lyrics::default().line_index_at(ms(5)), None);
    }

    #[test]
    fn plain_text_fallback() {
        let lyrics = Lyrics::from_text("  just words\nno stamps ");
        assert!(!lyrics.is_synced());
        assert_eq!(lyrics.plain.as_deref(), Some("just words\nno stamps"));
        assert!(Lyrics::from_text("   ").is_empty());
    }

    #[test]
    fn cleans_titles() {
        assert_eq!(clean_title("Song (Official Video) [HD]"), "Song");
        assert_eq!(clean_title("Song (feat. Someone)"), "Song (feat. Someone)");
        assert_eq!(clean_title("Video Games"), "Video Games");
        assert_eq!(clean_title("Song (Lyric Video"), "Song (Lyric Video");
    }

    #[test]
    fn best_record_prefers_synced_matching_duration() {
        let records: Vec<Record> = serde_json::from_str(
            r#"[{"duration":100.0,"plainLyrics":"p1","syncedLyrics":null},
                {"duration":300.0,"plainLyrics":"p2","syncedLyrics":"[00:01.00]far"},
                {"duration":201.0,"plainLyrics":"p3","syncedLyrics":"[00:01.00]near"},
                {"duration":200.0,"plainLyrics":"","syncedLyrics":""}]"#,
        )
        .unwrap();
        let best = best_record(records, Some(200))
            .unwrap()
            .into_lyrics()
            .unwrap();
        assert_eq!(best.synced[0].1, "near");
        assert_eq!(best.plain.as_deref(), Some("p3"));
        let none: Vec<Record> =
            serde_json::from_str(r#"[{"duration":1.0,"plainLyrics":null,"syncedLyrics":null}]"#)
                .unwrap();
        assert!(best_record(none, None).is_none());
    }

    #[test]
    fn instrumental_record() {
        let r: Record = serde_json::from_str(
            r#"{"id":1,"instrumental":true,"plainLyrics":null,"syncedLyrics":null,"duration":60}"#,
        )
        .unwrap();
        let l = r.into_lyrics().unwrap();
        assert!(l.instrumental && l.is_empty());
    }

    #[test]
    fn get_404_falls_back_to_search() {
        let search = r#"[{"id":7,"trackName":"Song","artistName":"Band","albumName":"LP",
            "duration":181.0,"instrumental":false,"plainLyrics":"hello\nworld",
            "syncedLyrics":"[00:01.00]hello\n[00:02.00]world"}]"#;
        let server = MockServer::start(vec![
            Reply::with_status(
                404,
                r#"{"code":404,"name":"TrackNotFound","message":"Failed to find specified track"}"#,
            ),
            Reply::json(search),
        ]);
        let client = LrclibClient::new(&server.base);
        let query = TrackQuery {
            artist: "Band",
            title: "Song (Official Video)",
            album: Some("LP"),
            duration: Some(180),
        };
        let lyrics = client.fetch(&agent(), &query).unwrap().unwrap();
        assert_eq!(lyrics.synced.len(), 2);
        let requests = server.requests();
        assert_eq!(
            request_line(&requests[0]),
            "GET /api/get?artist_name=Band&track_name=Song%20%28Official%20Video%29&album_name=LP&duration=180 HTTP/1.1"
        );
        assert_eq!(
            request_line(&requests[1]),
            "GET /api/search?track_name=Song&artist_name=Band HTTP/1.1"
        );
    }

    #[test]
    #[ignore = "network"]
    fn live_lrclib() {
        let query = TrackQuery {
            artist: "Borislav Slavov",
            title: "I Want to Live",
            album: Some("Baldur's Gate 3 (Original Game Soundtrack)"),
            duration: Some(233),
        };
        let lyrics = LrclibClient::default().fetch(&agent(), &query).unwrap();
        assert!(lyrics.is_some_and(|l| !l.is_empty()));
    }
}
