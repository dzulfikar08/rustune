//! Radio Browser (<https://www.radio-browser.info>) client: a free,
//! community-maintained directory of internet radio stations.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use ureq::Agent;

use super::{
    get_bytes, is_http_url, join_dot, non_empty, parse_json, push_encoded, push_query, random_u64,
    ProviderError, Result,
};
use crate::media::{MediaItem, SourceKind};

/// Lists the currently active API mirrors.
pub const DISCOVERY_URL: &str = "https://all.api.radio-browser.info/json/servers";

/// Known mirrors used when discovery fails.
pub const FALLBACK_SERVERS: &[&str] = &[
    "de1.api.radio-browser.info",
    "de2.api.radio-browser.info",
    "fi1.api.radio-browser.info",
];

/// The full tag index is the largest response (a few MiB).
const MAX_BODY: u64 = 16 << 20;
const DEFAULT_LIMIT: u32 = 50;

/// Radio settings for the application config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RadioConfig {
    pub enabled: bool,
    /// Fixed API host (e.g. `de1.api.radio-browser.info` or a full
    /// `https://` URL). Empty: discover a mirror at startup.
    pub server: String,
    /// ISO 3166-1 alpha-2 code for a "stations in my country" view.
    pub country: String,
}

impl Default for RadioConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            server: String::new(),
            country: String::new(),
        }
    }
}

/// Station listing order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StationOrder {
    #[default]
    Votes,
    Clicks,
    Trend,
}

impl StationOrder {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Votes => "votes",
            Self::Clicks => "clickcount",
            Self::Trend => "clicktrend",
        }
    }
}

/// Filters for `/json/stations/search`. The default lists the most voted
/// working stations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StationQuery {
    /// Free-text, partial station name.
    pub name: Option<String>,
    /// Exact tag (case-insensitive), e.g. `jazz`.
    pub tag: Option<String>,
    /// ISO 3166-1 alpha-2 code.
    pub country_code: Option<String>,
    pub order: StationOrder,
    pub offset: u32,
    /// 0 means the default page size (50).
    pub limit: u32,
}

impl StationQuery {
    pub fn by_name(name: &str) -> Self {
        Self {
            name: Some(name.to_string()),
            ..Self::default()
        }
    }

    pub fn by_tag(tag: &str) -> Self {
        Self {
            tag: Some(tag.to_string()),
            ..Self::default()
        }
    }

    pub fn by_country(code: &str) -> Self {
        Self {
            country_code: Some(code.to_string()),
            ..Self::default()
        }
    }

    /// Query string (without `?`). Broken stations are always hidden.
    pub fn to_query_string(&self) -> String {
        let limit = if self.limit == 0 {
            DEFAULT_LIMIT
        } else {
            self.limit
        };
        let limit = limit.to_string();
        let offset = self.offset.to_string();
        let mut q = String::with_capacity(128);
        push_query(
            &mut q,
            [
                ("limit", limit.as_str()),
                ("offset", offset.as_str()),
                ("order", self.order.as_str()),
                ("hidebroken", "true"),
                // Every order is "biggest first".
                ("reverse", "true"),
            ],
        );
        if let Some(name) = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            push_query(&mut q, [("name", name)]);
        }
        if let Some(tag) = self.tag.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            push_query(&mut q, [("tag", tag), ("tagExact", "true")]);
        }
        if let Some(code) = self
            .country_code
            .as_deref()
            .and_then(normalize_country_code)
        {
            push_query(&mut q, [("countrycode", code.as_str())]);
        }
        q.split_off(1) // drop the leading '?'
    }
}

/// One working station.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Station {
    pub uuid: String,
    pub name: String,
    /// Stream URL (`url_resolved` when available).
    pub url: String,
    pub homepage: Option<String>,
    pub favicon: Option<String>,
    pub tags: Vec<String>,
    pub country: Option<String>,
    pub country_code: Option<String>,
    pub codec: Option<String>,
    /// kbit/s; 0 when unknown.
    pub bitrate: u32,
    pub votes: u64,
    pub click_count: u64,
}

impl Station {
    /// Playable item: `id` is the stream URL, `subtitle` shows tags, codec
    /// and country.
    pub fn to_media_item(&self) -> MediaItem {
        let tags = (!self.tags.is_empty()).then(|| {
            let mut s = String::new();
            for (i, tag) in self.tags.iter().take(3).enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(tag);
            }
            s
        });
        let quality = match (self.codec.as_deref(), self.bitrate) {
            (Some(codec), 0) => Some(codec.to_string()),
            (Some(codec), rate) => Some(format!("{codec} {rate}k")),
            (None, 0) => None,
            (None, rate) => Some(format!("{rate}k")),
        };
        MediaItem {
            id: self.url.clone(),
            title: self.name.clone(),
            artist: None,
            album: None,
            duration: None,
            subtitle: join_dot([tags.as_deref(), quality.as_deref(), self.country.as_deref()]),
            source: SourceKind::Url,
        }
    }
}

/// Country index entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Country {
    pub name: String,
    /// Upper-case ISO 3166-1 alpha-2 code.
    pub code: String,
    pub station_count: u32,
}

/// Tag index entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    pub station_count: u32,
}

/// Client bound to one API mirror.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadioClient {
    base: String,
}

impl RadioClient {
    /// `server` is a host name (`de1.api.radio-browser.info`) or a full
    /// `http(s)://` base URL.
    pub fn new(server: &str) -> Self {
        let server = server.trim().trim_end_matches('/');
        let base = if is_http_url(server) {
            server.to_string()
        } else {
            format!("https://{server}")
        };
        Self { base }
    }

    /// Client from config: the fixed server when set, else discovery.
    pub fn from_config(agent: &Agent, cfg: &RadioConfig) -> Self {
        if cfg.server.trim().is_empty() {
            Self::discover(agent)
        } else {
            Self::new(&cfg.server)
        }
    }

    /// Picks a random live mirror from [`DISCOVERY_URL`], or a random entry
    /// of [`FALLBACK_SERVERS`] when discovery fails. Never errors.
    pub fn discover(agent: &Agent) -> Self {
        Self::discover_with(agent, DISCOVERY_URL)
    }

    /// [`discover`](Self::discover) against a custom discovery endpoint.
    pub fn discover_with(agent: &Agent, discovery_url: &str) -> Self {
        let servers = get_bytes(agent, discovery_url, "application/json", 1 << 20)
            .and_then(|bytes| parse_servers(&bytes))
            .unwrap_or_default();
        match pick(&servers) {
            Some(server) => Self::new(server),
            None => Self::new(pick(FALLBACK_SERVERS).unwrap_or(FALLBACK_SERVERS[0])),
        }
    }

    #[cfg(test)]
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// Runs a station search. Broken stations are dropped.
    pub fn search(&self, agent: &Agent, query: &StationQuery) -> Result<Vec<Station>> {
        let url = format!(
            "{}/json/stations/search?{}",
            self.base,
            query.to_query_string()
        );
        parse_stations(&self.get(agent, &url)?)
    }

    /// Most clicked / voted / trending working stations.
    pub fn top(&self, agent: &Agent, order: StationOrder, limit: u32) -> Result<Vec<Station>> {
        self.search(
            agent,
            &StationQuery {
                order,
                limit,
                ..StationQuery::default()
            },
        )
    }

    /// Countries with stations, most stations first.
    pub fn countries(&self, agent: &Agent) -> Result<Vec<Country>> {
        parse_countries(&self.get(agent, &format!("{}/json/countries", self.base))?)
    }

    /// Tags with working stations, most stations first. `limit` 0 = all.
    pub fn tags(&self, agent: &Agent, limit: u32) -> Result<Vec<Tag>> {
        let mut url = format!("{}/json/tags", self.base);
        let limit = if limit == 0 { 100_000 } else { limit };
        let limit = limit.to_string();
        push_query(
            &mut url,
            [
                ("order", "stationcount"),
                ("reverse", "true"),
                ("hidebroken", "true"),
                ("limit", limit.as_str()),
            ],
        );
        parse_tags(&self.get(agent, &url)?)
    }

    /// Reports a play to the directory (its popularity ranking depends on
    /// it) and returns the station's current stream URL.
    pub fn report_click(&self, agent: &Agent, station_uuid: &str) -> Result<Option<String>> {
        if station_uuid.is_empty()
            || !station_uuid
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b'-')
        {
            return Err(ProviderError::Invalid("station UUID".into()));
        }
        let mut url = format!("{}/json/url/", self.base);
        push_encoded(&mut url, station_uuid);
        parse_click(&self.get(agent, &url)?)
    }

    fn get(&self, agent: &Agent, url: &str) -> Result<Vec<u8>> {
        get_bytes(agent, url, "application/json", MAX_BODY)
    }
}

fn pick<S: AsRef<str>>(items: &[S]) -> Option<&str> {
    if items.is_empty() {
        return None;
    }
    let i = (random_u64() % items.len() as u64) as usize;
    Some(items[i].as_ref())
}

fn normalize_country_code(code: &str) -> Option<String> {
    let code = code.trim();
    (code.len() == 2 && code.bytes().all(|b| b.is_ascii_alphabetic()))
        .then(|| code.to_ascii_uppercase())
}

#[derive(Deserialize)]
struct RawServer {
    name: Option<String>,
}

/// Host names from `/json/servers`, deduplicated (the list repeats hosts
/// for IPv4 and IPv6), in response order.
pub fn parse_servers(bytes: &[u8]) -> Result<Vec<String>> {
    let raw: Vec<RawServer> = parse_json(bytes)?;
    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    for server in raw {
        let Some(name) = non_empty(server.name) else {
            continue;
        };
        let valid = name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
        if valid && !out.contains(&name) {
            out.push(name);
        }
    }
    Ok(out)
}

#[derive(Deserialize)]
struct RawStation {
    stationuuid: Option<String>,
    name: Option<String>,
    url: Option<String>,
    url_resolved: Option<String>,
    homepage: Option<String>,
    favicon: Option<String>,
    tags: Option<String>,
    country: Option<String>,
    countrycode: Option<String>,
    codec: Option<String>,
    bitrate: Option<i64>,
    votes: Option<i64>,
    clickcount: Option<i64>,
    lastcheckok: Option<serde_json::Value>,
}

fn non_negative<T: TryFrom<i64> + Default>(v: Option<i64>) -> T {
    v.and_then(|n| T::try_from(n).ok()).unwrap_or_default()
}

/// Parses a station list, dropping stations whose last check failed and
/// stations without an HTTP stream URL.
pub fn parse_stations(bytes: &[u8]) -> Result<Vec<Station>> {
    let raw: Vec<RawStation> = parse_json(bytes)?;
    Ok(raw.into_iter().filter_map(station_from_raw).collect())
}

fn station_from_raw(raw: RawStation) -> Option<Station> {
    let broken = match &raw.lastcheckok {
        Some(serde_json::Value::Number(n)) => n.as_i64() == Some(0),
        Some(serde_json::Value::Bool(ok)) => !ok,
        _ => false,
    };
    if broken {
        return None;
    }
    let url = non_empty(raw.url_resolved)
        .filter(|u| is_http_url(u))
        .or_else(|| non_empty(raw.url).filter(|u| is_http_url(u)))?;
    let tags = raw
        .tags
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    Some(Station {
        uuid: raw.stationuuid.unwrap_or_default(),
        name: non_empty(raw.name).unwrap_or_else(|| "Unnamed station".to_string()),
        url,
        homepage: non_empty(raw.homepage),
        favicon: non_empty(raw.favicon),
        tags,
        country: non_empty(raw.country),
        country_code: raw.countrycode.as_deref().and_then(normalize_country_code),
        codec: non_empty(raw.codec),
        bitrate: non_negative(raw.bitrate),
        votes: non_negative(raw.votes),
        click_count: non_negative(raw.clickcount),
    })
}

#[derive(Deserialize)]
struct RawCountry {
    name: Option<String>,
    iso_3166_1: Option<String>,
    stationcount: Option<i64>,
}

/// Parses `/json/countries`: entries without a valid code are dropped,
/// case-variant duplicates (`de` and `DE`) merged, most stations first.
pub fn parse_countries(bytes: &[u8]) -> Result<Vec<Country>> {
    let raw: Vec<RawCountry> = parse_json(bytes)?;
    let mut out: Vec<Country> = Vec::with_capacity(raw.len());
    let mut index: HashMap<String, usize> = HashMap::with_capacity(raw.len());
    for c in raw {
        let Some(code) = c.iso_3166_1.as_deref().and_then(normalize_country_code) else {
            continue;
        };
        let count: u32 = non_negative(c.stationcount);
        if let Some(&i) = index.get(&code) {
            out[i].station_count += count;
            continue;
        }
        index.insert(code.clone(), out.len());
        out.push(Country {
            name: non_empty(c.name).unwrap_or_else(|| code.clone()),
            code,
            station_count: count,
        });
    }
    out.retain(|c| c.station_count > 0);
    out.sort_by(|a, b| {
        b.station_count
            .cmp(&a.station_count)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(out)
}

#[derive(Deserialize)]
struct RawTag {
    name: Option<String>,
    stationcount: Option<i64>,
}

/// Parses `/json/tags`: blank and empty tags are dropped, case-variant
/// duplicates merged (the API matches tags case-insensitively), most
/// stations first.
pub fn parse_tags(bytes: &[u8]) -> Result<Vec<Tag>> {
    let raw: Vec<RawTag> = parse_json(bytes)?;
    let mut out: Vec<Tag> = Vec::with_capacity(raw.len());
    let mut index: HashMap<String, usize> = HashMap::with_capacity(raw.len());
    for t in raw {
        let count: u32 = non_negative(t.stationcount);
        let Some(name) = non_empty(t.name) else {
            continue;
        };
        if count == 0 {
            continue;
        }
        let key = name.to_lowercase();
        if let Some(&i) = index.get(&key) {
            out[i].station_count += count;
            continue;
        }
        index.insert(key, out.len());
        out.push(Tag {
            name,
            station_count: count,
        });
    }
    out.sort_by(|a, b| {
        b.station_count
            .cmp(&a.station_count)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

#[derive(Deserialize)]
struct RawClick {
    ok: Option<bool>,
    message: Option<String>,
    url: Option<String>,
}

fn parse_click(bytes: &[u8]) -> Result<Option<String>> {
    let raw: RawClick = parse_json(bytes)?;
    if raw.ok == Some(false) {
        return Err(ProviderError::Api {
            code: 0,
            message: raw.message.unwrap_or_else(|| "click rejected".into()),
        });
    }
    Ok(non_empty(raw.url).filter(|u| is_http_url(u)))
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{agent, request_line, MockServer, Reply};
    use super::*;

    const STATIONS: &str = r#"[
      {"changeuuid":"11ea52ff-ac0c-462e-b2a5-2ee446132f9b","stationuuid":"510506e7-6bc0-4b91-b6a1-fc024ccad1a8",
       "serveruuid":null,"name":"  SMOOTH JAZZ: cool jazz",
       "url":"https://playerservices.streamtheworld.com/api/livestream-redirect/ACIR22_s01AAC.aac",
       "url_resolved":"https://playerservices.streamtheworld.com/api/livestream-redirect/ACIR22_s01AAC.aac",
       "homepage":"https://www.iheart.com/live/smooth-jazz-8063/","favicon":"",
       "tags":"cool jazz,jazz, piano ,sax","country":"Mexico","countrycode":"MX","iso_3166_2":null,
       "state":"Ciudad de México","language":"spanish","votes":259,"codec":"AAC+","bitrate":48,"hls":0,
       "lastcheckok":1,"clickcount":52,"clicktrend":52,"ssl_error":0,"geo_lat":19.43,"geo_long":-99.13,
       "geo_distance":null,"has_extended_info":false},
      {"stationuuid":"b","name":"Dead FM","url":"http://dead.example/stream","url_resolved":"http://dead.example/stream",
       "lastcheckok":0,"votes":1000},
      {"stationuuid":"c","name":"Only URL","url":"http://only.example/live","url_resolved":"",
       "tags":"","codec":"MP3","bitrate":0,"votes":3,"lastcheckok":1},
      {"stationuuid":"d","name":"No URL","url":"","url_resolved":"","lastcheckok":1}
    ]"#;

    #[test]
    fn parses_stations_and_hides_broken() {
        let stations = parse_stations(STATIONS.as_bytes()).unwrap();
        assert_eq!(stations.len(), 2);
        let s = &stations[0];
        assert_eq!(s.name, "SMOOTH JAZZ: cool jazz");
        assert_eq!(s.uuid, "510506e7-6bc0-4b91-b6a1-fc024ccad1a8");
        assert_eq!(s.tags, ["cool jazz", "jazz", "piano", "sax"]);
        assert_eq!(s.country_code.as_deref(), Some("MX"));
        assert_eq!(s.favicon, None);
        assert_eq!((s.bitrate, s.votes, s.click_count), (48, 259, 52));
        assert_eq!(stations[1].url, "http://only.example/live");

        let item = s.to_media_item();
        assert_eq!(item.source, SourceKind::Url);
        assert_eq!(item.id, s.url);
        assert_eq!(
            item.subtitle.as_deref(),
            Some("cool jazz, jazz, piano · AAC+ 48k · Mexico")
        );
        assert_eq!(stations[1].to_media_item().subtitle.as_deref(), Some("MP3"));
    }

    #[test]
    fn query_string_encodes_and_defaults() {
        let q = StationQuery {
            name: Some(" rock & roll ".into()),
            tag: Some("80s".into()),
            country_code: Some("de".into()),
            ..StationQuery::default()
        };
        assert_eq!(
            q.to_query_string(),
            "limit=50&offset=0&order=votes&hidebroken=true&reverse=true&name=rock%20%26%20roll\
             &tag=80s&tagExact=true&countrycode=DE"
        );
        let q = StationQuery {
            order: StationOrder::Trend,
            limit: 10,
            offset: 20,
            country_code: Some("Germany".into()),
            ..StationQuery::default()
        };
        assert_eq!(
            q.to_query_string(),
            "limit=10&offset=20&order=clicktrend&hidebroken=true&reverse=true"
        );
    }

    #[test]
    fn parses_servers_dedup_and_validates() {
        let body = r#"[{"ip":"91.98.4.78","name":"de1.api.radio-browser.info"},
            {"ip":"2a01:4f8::1","name":"de1.api.radio-browser.info"},
            {"ip":"1.2.3.4","name":"fi1.api.radio-browser.info"},
            {"ip":"6.6.6.6","name":"evil.example/../x"},{"ip":"7.7.7.7"}]"#;
        assert_eq!(
            parse_servers(body.as_bytes()).unwrap(),
            ["de1.api.radio-browser.info", "fi1.api.radio-browser.info"]
        );
    }

    #[test]
    fn parses_countries_merging_case_duplicates() {
        let body = r#"[{"name":"Germany","iso_3166_1":"DE","stationcount":3000},
            {"name":"germany","iso_3166_1":"de","stationcount":5},
            {"name":"Atlantis","iso_3166_1":"","stationcount":9},
            {"name":"Brazil","iso_3166_1":"BR","stationcount":4000},
            {"name":"Empty","iso_3166_1":"EM","stationcount":0}]"#;
        let countries = parse_countries(body.as_bytes()).unwrap();
        assert_eq!(
            countries,
            vec![
                Country {
                    name: "Brazil".into(),
                    code: "BR".into(),
                    station_count: 4000
                },
                Country {
                    name: "Germany".into(),
                    code: "DE".into(),
                    station_count: 3005
                },
            ]
        );
    }

    #[test]
    fn parses_tags_merging_case_duplicates() {
        let body = r#"[{"name":"pop","stationcount":10},{"name":"Rock","stationcount":4},
            {"name":"rock","stationcount":8},{"name":"  ","stationcount":99},{"name":"zero","stationcount":0}]"#;
        let tags = parse_tags(body.as_bytes()).unwrap();
        assert_eq!(
            tags,
            vec![
                Tag {
                    name: "Rock".into(),
                    station_count: 12
                },
                Tag {
                    name: "pop".into(),
                    station_count: 10
                },
            ]
        );
    }

    #[test]
    fn click_response_errors_and_urls() {
        let ok = r#"{"ok":true,"message":"retrieved station url","stationuuid":"a","name":"x","url":"http://s.example/live"}"#;
        assert_eq!(
            parse_click(ok.as_bytes()).unwrap().as_deref(),
            Some("http://s.example/live")
        );
        let bad = r#"{"ok":false,"message":"station not found"}"#;
        assert!(matches!(
            parse_click(bad.as_bytes()),
            Err(ProviderError::Api { message, .. }) if message == "station not found"
        ));
    }

    #[test]
    fn client_base_url_forms() {
        assert_eq!(
            RadioClient::new("de1.api.radio-browser.info").base_url(),
            "https://de1.api.radio-browser.info"
        );
        assert_eq!(
            RadioClient::new("http://127.0.0.1:9/").base_url(),
            "http://127.0.0.1:9"
        );
    }

    #[test]
    fn report_click_rejects_bad_uuid_without_network() {
        let client = RadioClient::new("http://127.0.0.1:9");
        assert!(matches!(
            client.report_click(&agent(), "../servers"),
            Err(ProviderError::Invalid(_))
        ));
    }

    #[test]
    fn search_round_trip() {
        let server = MockServer::start(vec![Reply::json(STATIONS)]);
        let client = RadioClient::new(&server.base);
        let stations = client
            .search(&agent(), &StationQuery::by_tag("jazz"))
            .unwrap();
        assert_eq!(stations.len(), 2);
        let requests = server.requests();
        assert_eq!(
            request_line(&requests[0]),
            "GET /json/stations/search?limit=50&offset=0&order=votes&hidebroken=true&reverse=true&tag=jazz&tagExact=true HTTP/1.1"
        );
        assert!(requests[0]
            .to_ascii_lowercase()
            .contains(&format!("user-agent: {}", super::super::USER_AGENT)));
    }

    #[test]
    fn discovery_falls_back_when_unreachable() {
        let server = MockServer::start(vec![Reply::with_status(500, "oops")]);
        let client = RadioClient::discover_with(&agent(), &format!("{}/json/servers", server.base));
        let host = client.base_url().trim_start_matches("https://");
        assert!(FALLBACK_SERVERS.contains(&host), "{host}");
        server.requests();
    }

    #[test]
    #[ignore = "network"]
    fn live_top_stations() {
        let agent = agent();
        let client = RadioClient::discover(&agent);
        let stations = client.top(&agent, StationOrder::Clicks, 5).unwrap();
        assert!(!stations.is_empty());
    }
}
