//! Podcasts: iTunes Search API for discovery, Apple's top-charts feed per
//! country, and RSS feed parsing (quick-xml) for episodes.

use std::borrow::Cow;
use std::collections::HashSet;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use serde::{Deserialize, Serialize};
use ureq::Agent;

use super::{
    de_lenient_string, get_bytes, is_http_url, join_dot, non_empty, parse_json, push_query,
    ProviderError, Result,
};
use crate::media::{MediaItem, SourceKind};

pub const ITUNES_URL: &str = "https://itunes.apple.com";
/// Apple Marketing Tools RSS generator (successor of the iTunes RSS feeds):
/// `{CHARTS_URL}/{country}/podcasts/top/{limit}/podcasts.json`.
pub const CHARTS_URL: &str = "https://rss.marketingtools.apple.com/api/v2";

const MAX_JSON: u64 = 4 << 20;
const MAX_FEED: u64 = 32 << 20;
const FEED_ACCEPT: &str = "application/rss+xml, application/xml;q=0.9, text/xml;q=0.8, */*;q=0.5";

/// Podcast settings for the application config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PodcastConfig {
    pub enabled: bool,
    /// Store front for the top charts (ISO 3166-1 alpha-2, lower case).
    pub country: String,
    /// Subscribed feed URLs.
    pub subscriptions: Vec<String>,
}

impl Default for PodcastConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            country: "us".to_string(),
            subscriptions: Vec::new(),
        }
    }
}

/// A show from the directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Podcast {
    /// iTunes collection ID.
    pub id: String,
    pub title: String,
    pub author: Option<String>,
    pub feed_url: String,
    pub artwork: Option<String>,
    pub genre: Option<String>,
    pub episode_count: u32,
}

/// A parsed RSS feed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Feed {
    pub title: String,
    pub author: Option<String>,
    pub image: Option<String>,
    /// Newest first; undated episodes last, in feed order.
    pub episodes: Vec<Episode>,
}

/// One playable episode.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Episode {
    pub title: String,
    /// Enclosure URL.
    pub url: String,
    /// Enclosure MIME type.
    pub mime: Option<String>,
    /// Enclosure length in bytes.
    pub length: Option<u64>,
    /// `itunes:duration` in seconds.
    pub duration: Option<u64>,
    /// Publication time, seconds since the Unix epoch.
    pub published: Option<i64>,
    /// GUID, or the enclosure URL when the feed has none.
    pub guid: String,
    pub image: Option<String>,
}

impl Feed {
    /// Episodes as playable items, newest first.
    pub fn media_items(&self) -> Vec<MediaItem> {
        self.episodes
            .iter()
            .map(|e| e.to_media_item(self))
            .collect()
    }
}

impl Episode {
    /// `artist` is the show author (or title), `album` the show title and
    /// `subtitle` the publication date and length.
    pub fn to_media_item(&self, feed: &Feed) -> MediaItem {
        let date = self.published.map(format_date);
        let length = self.duration.filter(|&d| d > 0).map(format_hms);
        let show = (!feed.title.is_empty()).then(|| feed.title.clone());
        MediaItem {
            id: self.url.clone(),
            title: self.title.clone(),
            artist: feed.author.clone().or_else(|| show.clone()),
            album: show,
            duration: self.duration.filter(|&d| d > 0),
            subtitle: join_dot([date.as_deref(), length.as_deref()]),
            source: SourceKind::Url,
        }
    }
}

/// iTunes directory and Apple charts client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodcastClient {
    directory_url: String,
    charts_url: String,
}

impl Default for PodcastClient {
    fn default() -> Self {
        Self::new(ITUNES_URL, CHARTS_URL)
    }
}

impl PodcastClient {
    pub fn new(directory_url: &str, charts_url: &str) -> Self {
        Self {
            directory_url: directory_url.trim_end_matches('/').to_string(),
            charts_url: charts_url.trim_end_matches('/').to_string(),
        }
    }

    /// Searches shows by term (at most 200 results).
    pub fn search(&self, agent: &Agent, term: &str, limit: u32) -> Result<Vec<Podcast>> {
        let term = term.trim();
        if term.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 200).to_string();
        let mut url = format!("{}/search", self.directory_url);
        push_query(
            &mut url,
            [
                ("term", term),
                ("media", "podcast"),
                ("entity", "podcast"),
                ("limit", limit.as_str()),
            ],
        );
        parse_search(&get_bytes(agent, &url, "application/json", MAX_JSON)?)
    }

    /// Top shows of a country's store front (at most 100), in chart order.
    pub fn top(&self, agent: &Agent, country: &str, limit: u32) -> Result<Vec<Podcast>> {
        let country = country.trim();
        let country = if country.len() == 2 && country.bytes().all(|b| b.is_ascii_alphabetic()) {
            country.to_ascii_lowercase()
        } else {
            "us".to_string()
        };
        let url = format!(
            "{}/{country}/podcasts/top/{}/podcasts.json",
            self.charts_url,
            limit.clamp(1, 100)
        );
        let ids = parse_chart_ids(&get_bytes(agent, &url, "application/json", MAX_JSON)?)?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        // The chart lacks feed URLs; one lookup call resolves all of them.
        let joined = ids.join(",");
        let mut url = format!("{}/lookup", self.directory_url);
        push_query(&mut url, [("id", joined.as_str()), ("entity", "podcast")]);
        let mut shows = parse_search(&get_bytes(agent, &url, "application/json", MAX_JSON)?)?;
        let rank = |id: &str| ids.iter().position(|x| x == id).unwrap_or(usize::MAX);
        shows.sort_by_key(|s| rank(&s.id));
        Ok(shows)
    }
}

/// Downloads and parses a podcast RSS feed.
pub fn fetch_feed(agent: &Agent, feed_url: &str) -> Result<Feed> {
    let feed_url = feed_url.trim();
    if !is_http_url(feed_url) {
        return Err(ProviderError::Invalid(
            "feed URL must be an absolute http(s) URL".into(),
        ));
    }
    parse_feed(&get_bytes(agent, feed_url, FEED_ACCEPT, MAX_FEED)?)
}

#[derive(Deserialize)]
struct SearchResponse {
    results: Option<Vec<SearchEntry>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchEntry {
    #[serde(default, deserialize_with = "de_lenient_string")]
    collection_id: String,
    collection_name: Option<String>,
    artist_name: Option<String>,
    feed_url: Option<String>,
    artwork_url600: Option<String>,
    artwork_url100: Option<String>,
    primary_genre_name: Option<String>,
    track_count: Option<u32>,
}

/// Parses an iTunes `search`/`lookup` response. Entries without a usable
/// feed URL are dropped; shows are deduplicated by feed URL.
pub fn parse_search(bytes: &[u8]) -> Result<Vec<Podcast>> {
    let resp: SearchResponse = parse_json(bytes)?;
    let results = resp
        .results
        .ok_or_else(|| ProviderError::Parse("missing results array".into()))?;
    let mut seen = HashSet::with_capacity(results.len());
    let mut out = Vec::with_capacity(results.len());
    for entry in results {
        let Some(feed_url) = non_empty(entry.feed_url).filter(|u| is_http_url(u)) else {
            continue;
        };
        if !seen.insert(feed_url.clone()) {
            continue;
        }
        let artwork = non_empty(entry.artwork_url600)
            .filter(|u| is_http_url(u))
            .or_else(|| non_empty(entry.artwork_url100).filter(|u| is_http_url(u)));
        out.push(Podcast {
            id: entry.collection_id,
            title: non_empty(entry.collection_name).unwrap_or_else(|| "Untitled show".into()),
            author: non_empty(entry.artist_name),
            feed_url,
            artwork,
            genre: non_empty(entry.primary_genre_name),
            episode_count: entry.track_count.unwrap_or(0),
        });
    }
    Ok(out)
}

#[derive(Deserialize)]
struct ChartResponse {
    feed: ChartFeed,
}

#[derive(Deserialize)]
struct ChartFeed {
    results: Option<Vec<ChartEntry>>,
}

#[derive(Deserialize)]
struct ChartEntry {
    #[serde(default, deserialize_with = "de_lenient_string")]
    id: String,
}

/// Show IDs of an Apple charts response, in rank order.
pub fn parse_chart_ids(bytes: &[u8]) -> Result<Vec<String>> {
    let resp: ChartResponse = parse_json(bytes)?;
    let results = resp
        .feed
        .results
        .ok_or_else(|| ProviderError::Parse("missing results array".into()))?;
    Ok(results
        .into_iter()
        .map(|e| e.id.trim().to_string())
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        .collect())
}

// ---------------------------------------------------------------------------
// RSS
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    ChannelTitle,
    ChannelAuthor,
    ImageUrl,
    ItemTitle,
    Guid,
    PubDate,
    Duration,
}

#[derive(Default)]
struct ItemBuilder {
    episode: Episode,
    pub_date: Option<String>,
    duration: Option<String>,
}

/// Parses an RSS 2.0 podcast feed. Items without an audio enclosure are
/// skipped. Episodes are returned newest first.
pub fn parse_feed(bytes: &[u8]) -> Result<Feed> {
    let mut reader = Reader::from_reader(bytes);
    let mut feed = Feed::default();
    let mut seen_channel = false;
    let mut depth = 0usize;
    let mut in_channel = false;
    let mut item: Option<ItemBuilder> = None;
    let mut image_depth: Option<usize> = None; // channel <image> block
    let mut capture: Option<(Field, usize)> = None;
    let mut text = String::new();

    loop {
        let event = reader
            .read_event()
            .map_err(|e| ProviderError::Parse(format!("feed XML: {e}")))?;
        match event {
            Event::Start(e) => {
                depth += 1;
                let local = e.local_name();
                let local = local.as_ref();
                if !in_channel {
                    if local == b"channel" {
                        in_channel = true;
                        seen_channel = true;
                    }
                    continue;
                }
                if capture.is_some() {
                    continue; // markup inside a text field (rare); keep its text
                }
                match (item.is_some(), local) {
                    (false, b"item") => item = Some(ItemBuilder::default()),
                    (true, b"enclosure") => enclosure(&e, item.as_mut()),
                    (_, b"image") => {
                        if let Some(href) = attr(&e, b"href") {
                            set_image(&mut feed, item.as_mut(), href);
                        } else if item.is_none() {
                            image_depth = Some(depth);
                        }
                    }
                    _ => {
                        if let Some(field) =
                            field_for(local, e.name().as_ref(), item.is_some(), image_depth)
                        {
                            text.clear();
                            capture = Some((field, depth));
                        }
                    }
                }
            }
            Event::Empty(e) => {
                if !in_channel || capture.is_some() {
                    continue;
                }
                match e.local_name().as_ref() {
                    b"enclosure" => enclosure(&e, item.as_mut()),
                    b"image" => {
                        if let Some(href) = attr(&e, b"href") {
                            set_image(&mut feed, item.as_mut(), href);
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(t) => {
                if capture.is_some() {
                    text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::CData(t) => {
                if capture.is_some() {
                    text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::GeneralRef(r) => {
                if capture.is_some() {
                    push_entity(&mut text, &r);
                }
            }
            Event::End(e) => {
                if let Some((field, d)) = capture {
                    if d == depth {
                        store(&mut feed, item.as_mut(), field, text.trim());
                        capture = None;
                    }
                }
                if image_depth == Some(depth) {
                    image_depth = None;
                }
                if in_channel {
                    match e.local_name().as_ref() {
                        b"item" if item.is_some() => {
                            if let Some(ep) = item.take().and_then(finish_item) {
                                feed.episodes.push(ep);
                            }
                        }
                        b"channel" => in_channel = false,
                        _ => {}
                    }
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !seen_channel {
        return Err(ProviderError::Parse(
            "not an RSS feed (no <channel>)".into(),
        ));
    }
    if feed.title.is_empty() {
        feed.title = "Untitled show".to_string();
    }
    // Stable: undated episodes (None sorts lowest) keep feed order at the end.
    feed.episodes
        .sort_by_key(|e| std::cmp::Reverse(e.published));
    Ok(feed)
}

fn field_for(
    local: &[u8],
    qname: &[u8],
    in_item: bool,
    image_depth: Option<usize>,
) -> Option<Field> {
    if image_depth.is_some() {
        return (local == b"url").then_some(Field::ImageUrl);
    }
    match (in_item, local) {
        (false, b"title") => Some(Field::ChannelTitle),
        // Channel-level <author> only exists as itunes:author / googleplay:author.
        (false, b"author") if qname != b"author" => Some(Field::ChannelAuthor),
        (true, b"title") => Some(Field::ItemTitle),
        (true, b"guid") => Some(Field::Guid),
        (true, b"pubDate") => Some(Field::PubDate),
        (true, b"duration") => Some(Field::Duration),
        _ => None,
    }
}

fn store(feed: &mut Feed, item: Option<&mut ItemBuilder>, field: Field, value: &str) {
    if value.is_empty() {
        return;
    }
    match (field, item) {
        (Field::ChannelTitle, None) if feed.title.is_empty() => feed.title = value.to_string(),
        (Field::ChannelAuthor, None) if feed.author.is_none() => {
            feed.author = Some(value.to_string());
        }
        (Field::ImageUrl, None) if feed.image.is_none() && is_http_url(value) => {
            feed.image = Some(value.to_string());
        }
        (Field::ItemTitle, Some(it)) => it.episode.title = value.to_string(),
        (Field::Guid, Some(it)) => it.episode.guid = value.to_string(),
        (Field::PubDate, Some(it)) => it.pub_date = Some(value.to_string()),
        (Field::Duration, Some(it)) => it.duration = Some(value.to_string()),
        _ => {}
    }
}

fn set_image(feed: &mut Feed, item: Option<&mut ItemBuilder>, href: String) {
    if !is_http_url(&href) {
        return;
    }
    match item {
        Some(it) if it.episode.image.is_none() => it.episode.image = Some(href),
        None if feed.image.is_none() => feed.image = Some(href),
        _ => {}
    }
}

/// Keeps the first audio enclosure of an item.
fn enclosure(e: &BytesStart<'_>, item: Option<&mut ItemBuilder>) {
    let Some(it) = item else { return };
    if !it.episode.url.is_empty() {
        return;
    }
    let Some(url) = attr(e, b"url").map(|u| u.trim().to_string()) else {
        return;
    };
    if !is_http_url(&url) {
        return;
    }
    let mime = attr(e, b"type")
        .map(|t| {
            t.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .filter(|t| !t.is_empty());
    let audio = match mime.as_deref() {
        None | Some("application/octet-stream" | "binary/octet-stream") => {
            has_audio_extension(&url)
        }
        Some(m) => m.starts_with("audio/") || m == "application/ogg",
    };
    if !audio {
        return;
    }
    it.episode.length = attr(e, b"length")
        .and_then(|l| l.trim().parse().ok())
        .filter(|&l| l > 0);
    it.episode.mime = mime;
    it.episode.url = url;
}

fn has_audio_extension(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let Some((_, ext)) = path.rsplit_once('.') else {
        return false;
    };
    const AUDIO: &[&str] = &[
        "mp3", "m4a", "m4b", "aac", "ogg", "oga", "opus", "flac", "wav",
    ];
    AUDIO.iter().any(|a| ext.eq_ignore_ascii_case(a))
}

fn finish_item(b: ItemBuilder) -> Option<Episode> {
    let mut ep = b.episode;
    if ep.url.is_empty() {
        return None;
    }
    if ep.title.is_empty() {
        ep.title = "Untitled episode".to_string();
    }
    if ep.guid.is_empty() {
        ep.guid.clone_from(&ep.url);
    }
    ep.duration = b.duration.as_deref().and_then(parse_duration);
    ep.published = b.pub_date.as_deref().and_then(parse_pub_date);
    Some(ep)
}

/// Attribute value; falls back to the raw text when it holds stray `&`
/// (common in enclosure URLs of hand-made feeds).
fn attr(e: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    let a = e.try_get_attribute(name).ok()??;
    match a.unescape_value() {
        Ok(v) => Some(v.into_owned()),
        Err(_) => Some(String::from_utf8_lossy(&a.value).into_owned()),
    }
}

fn push_entity(text: &mut String, r: &quick_xml::events::BytesRef<'_>) {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        text.push(c);
        return;
    }
    let name: Cow<'_, str> = String::from_utf8_lossy(r);
    match quick_xml::escape::resolve_predefined_entity(&name) {
        Some(s) => text.push_str(s),
        // HTML entities are invalid XML but frequent in feeds.
        None if name == "nbsp" => text.push(' '),
        None => {
            text.push('&');
            text.push_str(&name);
            text.push(';');
        }
    }
}

/// Parses `itunes:duration`: plain seconds (fractions allowed), `mm:ss`
/// or `hh:mm:ss`.
pub fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut parts = s.rsplit(':');
    let secs_part = parts.next()?;
    let secs: f64 = secs_part.trim().parse().ok()?;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    let mut total = secs as u64;
    let mut multiplier = 60u64;
    for (i, part) in parts.enumerate() {
        if i >= 2 {
            return None;
        }
        let part = part.trim();
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let n: u64 = part.parse().ok()?;
        total = total.checked_add(n.checked_mul(multiplier)?)?;
        multiplier *= 60;
    }
    Some(total)
}

/// Parses an RFC 2822 date (`Fri, 23 Oct 2026 09:45:00 GMT`, with the usual
/// feed sloppiness: missing weekday or seconds, full month names, two-digit
/// years, named US zones) or an RFC 3339 / ISO 8601 date. Returns Unix
/// seconds.
pub fn parse_pub_date(s: &str) -> Option<i64> {
    let s = s.trim();
    parse_rfc2822(s).or_else(|| parse_rfc3339(s))
}

fn parse_rfc2822(s: &str) -> Option<i64> {
    let mut tokens = s
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .peekable();
    if tokens
        .peek()?
        .starts_with(|c: char| c.is_ascii_alphabetic())
    {
        tokens.next(); // weekday
    }
    let day: u32 = tokens.next()?.parse().ok()?;
    let month = month_number(tokens.next()?)?;
    let mut year: i64 = tokens.next()?.parse().ok()?;
    if year < 100 {
        year += if year < 50 { 2000 } else { 1900 };
    }
    let (h, m, sec) = match tokens.next() {
        Some(t) => parse_clock(t)?,
        None => (0, 0, 0),
    };
    let offset = tokens.next().map_or(0, zone_offset);
    to_unix(year, month, day, h, m, sec).map(|t| t - offset)
}

fn parse_rfc3339(s: &str) -> Option<i64> {
    let (date, rest) = match s.find(['T', 't', ' ']) {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    };
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: u32 = d.next()?.parse().ok()?;
    let day: u32 = d.next()?.parse().ok()?;
    if d.next().is_some() {
        return None;
    }
    if rest.is_empty() {
        return to_unix(year, month, day, 0, 0, 0);
    }
    let zone_at = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
    let clock = rest[..zone_at].split('.').next().unwrap_or("");
    let (h, m, sec) = parse_clock(clock)?;
    let offset = zone_offset(&rest[zone_at..]);
    to_unix(year, month, day, h, m, sec).map(|t| t - offset)
}

fn parse_clock(t: &str) -> Option<(u32, u32, u32)> {
    let mut it = t.split(':');
    let h: u32 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let s: u32 = match it.next() {
        Some(s) => s.parse().ok()?,
        None => 0,
    };
    (h < 24 && m < 60 && s <= 60 && it.next().is_none()).then_some((h, m, s))
}

fn month_number(name: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let head = name.get(..3)?;
    MONTHS
        .iter()
        .position(|m| head.eq_ignore_ascii_case(m))
        .map(|i| i as u32 + 1)
}

/// Seconds east of UTC; unknown zones count as UTC.
fn zone_offset(zone: &str) -> i64 {
    let zone = zone.trim();
    if let Some(sign) = zone.chars().next().filter(|c| *c == '+' || *c == '-') {
        let digits: String = zone[1..].chars().filter(char::is_ascii_digit).collect();
        if digits.len() != 4 {
            return 0;
        }
        let h: i64 = digits[..2].parse().unwrap_or(0);
        let m: i64 = digits[2..].parse().unwrap_or(0);
        let secs = h * 3600 + m * 60;
        return if sign == '-' { -secs } else { secs };
    }
    let hours = match zone.to_ascii_uppercase().as_str() {
        "EDT" => -4,
        "EST" | "CDT" => -5,
        "CST" | "MDT" => -6,
        "MST" | "PDT" => -7,
        "PST" => -8,
        _ => 0,
    };
    hours * 3600
}

fn to_unix(year: i64, month: u32, day: u32, h: u32, m: u32, s: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + i64::from(h * 3600 + m * 60 + s))
}

/// Days since 1970-01-01 (proleptic Gregorian), after Howard Hinnant.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `YYYY-MM-DD` (UTC) for Unix seconds.
pub fn format_date(unix: i64) -> String {
    let (y, m, d) = civil_from_days(unix.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// `m:ss` or `h:mm:ss`.
pub fn format_hms(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{agent, request_line, MockServer, Reply};
    use super::*;

    const FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd" xmlns:atom="http://www.w3.org/2005/Atom">
  <channel>
    <atom:link href="https://feeds.example.com/show" rel="self" type="application/rss+xml"/>
    <title>Tom &amp; Jerry&#8217;s Show</title>
    <itunes:author>ACME Media</itunes:author>
    <image>
      <url>https://cdn.example.com/rss-image.jpg</url>
      <title>ignored image title</title>
    </image>
    <itunes:image href="https://cdn.example.com/itunes.jpg"/>
    <itunes:owner><itunes:name>Owner</itunes:name></itunes:owner>
    <item>
      <title>Episode 1</title>
      <guid isPermaLink="false">ep-1</guid>
      <pubDate>Mon, 05 Oct 2026 10:00:00 +0000</pubDate>
      <itunes:duration>1:02:03</itunes:duration>
      <enclosure url="https://cdn.example.com/ep1.mp3?a=1&amp;b=2" length="12345" type="audio/mpeg"/>
    </item>
    <item>
      <title><![CDATA[Episode <2>]]></title>
      <pubDate>Fri, 23 Oct 2026 09:45:00 GMT</pubDate>
      <itunes:duration>45:30</itunes:duration>
      <itunes:image href="https://cdn.example.com/ep2.jpg"/>
      <enclosure url="https://cdn.example.com/ep2.m4a" length="0" type="audio/x-m4a; charset=binary"></enclosure>
    </item>
    <item>
      <title>Video only</title>
      <pubDate>Sat, 24 Oct 2026 09:45:00 GMT</pubDate>
      <enclosure url="https://cdn.example.com/ep.mp4" type="video/mp4"/>
    </item>
    <item>
      <title>Undated</title>
      <itunes:duration>3600</itunes:duration>
      <enclosure url="https://cdn.example.com/raw.ogg" type="application/octet-stream"/>
    </item>
    <item>
      <title>No enclosure</title>
    </item>
    <item>
      <pubDate>Thu, 01 Oct 2026 08:00:00 EST</pubDate>
      <enclosure url="https://cdn.example.com/ep0.mp3?x=1&y=2" type="audio/mpeg"/>
    </item>
  </channel>
</rss>"#;

    #[test]
    fn parses_feed_metadata_and_orders_episodes() {
        let feed = parse_feed(FEED.as_bytes()).unwrap();
        assert_eq!(feed.title, "Tom & Jerry\u{2019}s Show");
        assert_eq!(feed.author.as_deref(), Some("ACME Media"));
        assert_eq!(
            feed.image.as_deref(),
            Some("https://cdn.example.com/rss-image.jpg")
        );
        let titles: Vec<&str> = feed.episodes.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Episode <2>", "Episode 1", "Untitled episode", "Undated"]
        );

        let ep2 = &feed.episodes[0];
        assert_eq!(ep2.url, "https://cdn.example.com/ep2.m4a");
        assert_eq!(ep2.mime.as_deref(), Some("audio/x-m4a"));
        assert_eq!(ep2.length, None);
        assert_eq!(ep2.duration, Some(45 * 60 + 30));
        assert_eq!(ep2.guid, ep2.url);
        assert_eq!(
            ep2.image.as_deref(),
            Some("https://cdn.example.com/ep2.jpg")
        );
        assert_eq!(format_date(ep2.published.unwrap()), "2026-10-23");

        let ep1 = &feed.episodes[1];
        assert_eq!(ep1.url, "https://cdn.example.com/ep1.mp3?a=1&b=2");
        assert_eq!(ep1.guid, "ep-1");
        assert_eq!(ep1.length, Some(12345));
        assert_eq!(ep1.duration, Some(3723));

        // Stray '&' in an attribute falls back to the raw value.
        assert_eq!(
            feed.episodes[2].url,
            "https://cdn.example.com/ep0.mp3?x=1&y=2"
        );
        assert_eq!(feed.episodes[2].published, Some(1_790_841_600 + 5 * 3600));
        assert_eq!(feed.episodes[3].published, None);
        assert_eq!(feed.episodes[3].duration, Some(3600));

        let item = ep1.to_media_item(&feed);
        assert_eq!(item.source, SourceKind::Url);
        assert_eq!(item.artist.as_deref(), Some("ACME Media"));
        assert_eq!(item.album.as_deref(), Some("Tom & Jerry\u{2019}s Show"));
        assert_eq!(item.duration, Some(3723));
        assert_eq!(item.subtitle.as_deref(), Some("2026-10-05 · 1:02:03"));
        assert_eq!(feed.media_items().len(), 4);
    }

    #[test]
    fn rejects_non_rss() {
        assert!(parse_feed(b"<html><body>nope</body></html>").is_err());
        assert!(parse_feed(b"<rss><channel><title>x</title></rss>").is_err());
    }

    #[test]
    fn duration_formats() {
        assert_eq!(parse_duration("3600"), Some(3600));
        assert_eq!(parse_duration(" 61.9 "), Some(61));
        assert_eq!(parse_duration("05:07"), Some(307));
        assert_eq!(parse_duration("1:00:01"), Some(3601));
        assert_eq!(parse_duration("90:00"), Some(5400));
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("1:2:3:4"), None);
        assert_eq!(parse_duration("-5"), None);
        assert_eq!(parse_duration("x:30"), None);
    }

    #[test]
    fn pub_date_formats() {
        let base = 1_792_748_700; // 2026-10-23T09:45:00Z
        assert_eq!(parse_pub_date("Fri, 23 Oct 2026 09:45:00 GMT"), Some(base));
        assert_eq!(
            parse_pub_date("Fri, 23 Oct 2026 09:45:00 +0000"),
            Some(base)
        );
        assert_eq!(parse_pub_date("23 Oct 2026 11:45:00 +0200"), Some(base));
        assert_eq!(parse_pub_date("Fri, 23 October 2026 05:45 EDT"), Some(base));
        assert_eq!(parse_pub_date("Fri, 23 Oct 26 02:45:00 PDT"), Some(base));
        assert_eq!(parse_pub_date("2026-10-23T09:45:00Z"), Some(base));
        assert_eq!(parse_pub_date("2026-10-23T11:45:00.123+02:00"), Some(base));
        assert_eq!(parse_pub_date("2026-10-23"), Some(base - 35_100));
        assert_eq!(parse_pub_date("yesterday"), None);
        assert_eq!(parse_pub_date("Fri, 32 Oct 2026 09:45:00 GMT"), None);
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(format_date(951_782_400), "2000-02-29");
        assert_eq!(format_date(-86_400), "1969-12-31");
    }

    #[test]
    fn hms_formatting() {
        assert_eq!(format_hms(59), "0:59");
        assert_eq!(format_hms(3723), "1:02:03");
    }

    #[test]
    fn parses_itunes_search() {
        let body = r#"{"resultCount":3,"results":[
          {"wrapperType":"track","kind":"podcast","collectionId":1200361736,"trackId":1200361736,
           "artistName":"The New York Times","collectionName":"The Daily",
           "feedUrl":"https://feeds.simplecast.com/Sl5CSM3S",
           "artworkUrl100":"https://is1-ssl.mzstatic.com/a/100x100bb.jpg",
           "artworkUrl600":"https://is1-ssl.mzstatic.com/a/600x600bb.jpg",
           "trackCount":2759,"primaryGenreName":"Daily News"},
          {"collectionId":2,"collectionName":"Dup","feedUrl":"https://feeds.simplecast.com/Sl5CSM3S"},
          {"collectionId":3,"collectionName":"No feed"}]}"#;
        let shows = parse_search(body.as_bytes()).unwrap();
        assert_eq!(
            shows,
            vec![Podcast {
                id: "1200361736".into(),
                title: "The Daily".into(),
                author: Some("The New York Times".into()),
                feed_url: "https://feeds.simplecast.com/Sl5CSM3S".into(),
                artwork: Some("https://is1-ssl.mzstatic.com/a/600x600bb.jpg".into()),
                genre: Some("Daily News".into()),
                episode_count: 2759,
            }]
        );
        assert!(parse_search(br#"{"resultCount":0}"#).is_err());
    }

    #[test]
    fn parses_chart_ids() {
        let body = r#"{"feed":{"title":"Top Shows","country":"us","results":[
          {"artistName":"The New York Times","id":"1200361736","name":"The Daily","kind":"podcasts"},
          {"id":"bogus"},{"id":"1028908750","name":"Up First"}]}}"#;
        assert_eq!(
            parse_chart_ids(body.as_bytes()).unwrap(),
            ["1200361736", "1028908750"]
        );
    }

    #[test]
    fn top_round_trip_keeps_chart_order() {
        let chart = r#"{"feed":{"results":[{"id":"2"},{"id":"1"}]}}"#;
        let lookup = r#"{"resultCount":2,"results":[
          {"collectionId":1,"collectionName":"One","feedUrl":"https://a.example/1"},
          {"collectionId":2,"collectionName":"Two","feedUrl":"https://a.example/2"}]}"#;
        let server = MockServer::start(vec![Reply::json(chart), Reply::json(lookup)]);
        let client = PodcastClient::new(&server.base, &server.base);
        let shows = client.top(&agent(), "GB", 25).unwrap();
        let titles: Vec<&str> = shows.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Two", "One"]);
        let requests = server.requests();
        assert_eq!(
            request_line(&requests[0]),
            "GET /gb/podcasts/top/25/podcasts.json HTTP/1.1"
        );
        assert_eq!(
            request_line(&requests[1]),
            "GET /lookup?id=2%2C1&entity=podcast HTTP/1.1"
        );
    }

    #[test]
    fn feed_round_trip() {
        let server = MockServer::start(vec![Reply::xml(FEED)]);
        let feed = fetch_feed(&agent(), &format!("{}/feed.xml", server.base)).unwrap();
        assert_eq!(feed.episodes.len(), 4);
        server.requests();
        assert!(matches!(
            fetch_feed(&agent(), "file:///etc/passwd"),
            Err(ProviderError::Invalid(_))
        ));
    }

    #[test]
    #[ignore = "network"]
    fn live_search_and_feed() {
        let agent = agent();
        let shows = PodcastClient::default()
            .search(&agent, "the daily", 5)
            .unwrap();
        let feed = fetch_feed(&agent, &shows[0].feed_url).unwrap();
        assert!(!feed.episodes.is_empty());
        assert!(!PodcastClient::default()
            .top(&agent, "us", 10)
            .unwrap()
            .is_empty());
    }
}
