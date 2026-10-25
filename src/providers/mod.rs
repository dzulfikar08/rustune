//! Blocking clients for online sources: internet radio, podcasts, lyrics and
//! self-hosted media servers (Subsonic/Navidrome, Jellyfin/Emby).
//!
//! Every client takes a borrowed [`ureq::Agent`] so the application owns the
//! single connection pool and its timeouts. The calls block; run them on a
//! blocking thread (`tokio::task::spawn_blocking`). Playable results are
//! [`MediaItem`](crate::media::MediaItem)s with `SourceKind::Url`, containers
//! (albums, playlists, shows, countries…) are small typed structs.
//!
//! Credentials never appear in errors or `Debug` output.

pub mod jellyfin;
pub mod lyrics;
pub mod podcast;
pub mod radio;
pub mod subsonic;

use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

pub use jellyfin::JellyfinConfig;
pub use lyrics::LyricsConfig;
pub use podcast::PodcastConfig;
pub use radio::RadioConfig;
pub use subsonic::SubsonicConfig;

/// `User-Agent` sent by every provider request.
pub const USER_AGENT: &str = concat!("rustune/", env!("CARGO_PKG_VERSION"));

/// Failure of a provider call. Messages never contain URLs or credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// Transport failure: DNS, connect, TLS, timeout, truncated body.
    Network(String),
    /// The server answered with a non-success HTTP status.
    Status(u16),
    /// The response body could not be understood.
    Parse(String),
    /// The service reported an error inside a well-formed response
    /// (e.g. a Subsonic `status: failed` envelope).
    Api { code: i64, message: String },
    /// Credentials are missing or were rejected.
    Auth(String),
    /// The request was refused before it was sent (bad input such as a
    /// non-HTTP feed URL).
    Invalid(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(msg) => write!(f, "network error: {msg}"),
            Self::Status(code) => write!(f, "HTTP status {code}"),
            Self::Parse(msg) => write!(f, "unexpected response: {msg}"),
            Self::Api { code, message } => write!(f, "{message} (code {code})"),
            Self::Auth(msg) => write!(f, "authentication failed: {msg}"),
            Self::Invalid(msg) => write!(f, "invalid request: {msg}"),
        }
    }
}

impl std::error::Error for ProviderError {}

impl From<ureq::Error> for ProviderError {
    fn from(err: ureq::Error) -> Self {
        match err {
            ureq::Error::StatusCode(code) => Self::Status(code),
            ureq::Error::Json(e) => Self::Parse(e.to_string()),
            // These variants can carry the request URL, which may hold an
            // auth token (Subsonic, Jellyfin stream URLs).
            ureq::Error::BadUri(_) | ureq::Error::Http(_) => Self::Invalid("malformed URL".into()),
            ureq::Error::RequireHttpsOnly(_) => Self::Invalid("HTTPS required".into()),
            ureq::Error::ConnectProxyFailed(_) => Self::Network("proxy connection failed".into()),
            ureq::Error::BodyExceedsLimit(limit) => {
                Self::Network(format!("response larger than {limit} bytes"))
            }
            other => Self::Network(other.to_string()),
        }
    }
}

/// Result alias for provider calls.
pub type Result<T, E = ProviderError> = std::result::Result<T, E>;

/// The browsable online sources, in menu order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Radio,
    Podcast,
    Subsonic,
    Jellyfin,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 4] = [
        ProviderKind::Radio,
        ProviderKind::Podcast,
        ProviderKind::Subsonic,
        ProviderKind::Jellyfin,
    ];

    /// Human-readable name for menus and status lines.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Radio => "Internet Radio",
            Self::Podcast => "Podcasts",
            Self::Subsonic => "Subsonic / Navidrome",
            Self::Jellyfin => "Jellyfin",
        }
    }

    /// Whether the provider needs a user-configured server and credentials.
    pub fn needs_server(self) -> bool {
        matches!(self, Self::Subsonic | Self::Jellyfin)
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

/// All provider settings, meant to be embedded as a `[providers]` table of
/// the application config.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProvidersConfig {
    pub radio: RadioConfig,
    pub podcast: PodcastConfig,
    pub lyrics: LyricsConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subsonic: Option<SubsonicConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jellyfin: Option<JellyfinConfig>,
}

impl ProvidersConfig {
    /// Providers that are enabled and, for servers, configured.
    pub fn enabled(&self) -> Vec<ProviderKind> {
        ProviderKind::ALL
            .into_iter()
            .filter(|kind| match kind {
                ProviderKind::Radio => self.radio.enabled,
                ProviderKind::Podcast => self.podcast.enabled,
                ProviderKind::Subsonic => self.subsonic.as_ref().is_some_and(|c| c.is_configured()),
                ProviderKind::Jellyfin => self.jellyfin.as_ref().is_some_and(|c| c.is_configured()),
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

type HttpResult = Result<ureq::http::Response<ureq::Body>, ureq::Error>;

/// Checks the status of a finished request and reads at most `limit` bytes
/// of its body.
pub(crate) fn read_ok(result: HttpResult, limit: u64) -> Result<Vec<u8>> {
    let mut resp = result?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(ProviderError::Status(status));
    }
    Ok(resp.body_mut().with_config().limit(limit).read_to_vec()?)
}

/// GET `url` with the rustune User-Agent and the given `Accept` header.
pub(crate) fn get_bytes(
    agent: &ureq::Agent,
    url: &str,
    accept: &str,
    limit: u64,
) -> Result<Vec<u8>> {
    read_ok(
        agent
            .get(url)
            .header("User-Agent", USER_AGENT)
            .header("Accept", accept)
            .call(),
        limit,
    )
}

pub(crate) fn parse_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|e| ProviderError::Parse(e.to_string()))
}

/// Accepts JSON strings and numbers (some servers send numeric IDs); null
/// and anything else become an empty string.
pub(crate) fn de_lenient_string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) => s,
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        _ => String::new(),
    })
}

/// Accepts an array, a single object (old Subsonic servers collapse
/// one-element lists) or null.
pub(crate) fn de_one_or_many<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    match serde_json::Value::deserialize(d)? {
        serde_json::Value::Null => Ok(Vec::new()),
        v @ serde_json::Value::Array(_) => {
            serde_json::from_value(v).map_err(serde::de::Error::custom)
        }
        v => serde_json::from_value(v)
            .map(|one| vec![one])
            .map_err(serde::de::Error::custom),
    }
}

// ---------------------------------------------------------------------------
// URL helpers
// ---------------------------------------------------------------------------

/// Appends `s` percent-encoded (RFC 3986 unreserved characters pass through).
pub(crate) fn push_encoded(out: &mut String, s: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[usize::from(b >> 4)] as char);
            out.push(HEX[usize::from(b & 0x0f)] as char);
        }
    }
}

/// Appends `key=value` pairs to `url`, choosing `?` or `&` as needed.
pub(crate) fn push_query<'a>(
    url: &mut String,
    pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
) {
    let mut has_query = url.contains('?');
    for (key, value) in pairs {
        if !has_query {
            url.push('?');
            has_query = true;
        } else if !url.ends_with('?') && !url.ends_with('&') {
            url.push('&');
        }
        push_encoded(url, key);
        url.push('=');
        push_encoded(url, value);
    }
}

/// Decodes `%XX` escapes and `+` (form encoding). Invalid escapes are kept.
pub(crate) fn percent_decode(s: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push((h << 4) | l);
                    i += 3;
                    continue;
                }
                _ => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Returns the decoded value of the first `key` query parameter of `url`.
pub(crate) fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    let query = query.split('#').next().unwrap_or(query);
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (percent_decode(k) == key).then(|| percent_decode(v))
    })
}

/// True for absolute `http`/`https` URLs with a host and no userinfo.
pub(crate) fn is_http_url(url: &str) -> bool {
    let rest = if let Some(r) = strip_prefix_ignore_case(url, "https://") {
        r
    } else if let Some(r) = strip_prefix_ignore_case(url, "http://") {
        r
    } else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    !authority.is_empty() && !authority.contains('@') && !authority.starts_with(':')
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// Normalizes a user-entered server URL: trims whitespace and trailing `/`.
pub(crate) fn normalize_base(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

// ---------------------------------------------------------------------------
// Misc
// ---------------------------------------------------------------------------

/// Unpredictable 64-bit value without the `rand` crate: std's `RandomState`
/// is seeded from the OS once per thread and re-keyed on every call; the
/// clock and a counter make consecutive values distinct.
pub(crate) fn random_u64() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
        hasher.write_u128(now.as_nanos());
    }
    hasher.finish()
}

pub(crate) fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

/// Joins the non-empty parts with `" · "`; `None` when all are empty.
pub(crate) fn join_dot<'a>(parts: impl IntoIterator<Item = Option<&'a str>>) -> Option<String> {
    let mut out = String::new();
    for part in parts.into_iter().flatten() {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str(" · ");
        }
        out.push_str(part);
    }
    (!out.is_empty()).then_some(out)
}

/// `Some(trimmed)` for non-blank strings.
pub(crate) fn non_empty(s: Option<String>) -> Option<String> {
    let s = s?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else if trimmed.len() == s.len() {
        Some(s)
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    //! One-shot in-process HTTP server for round-trip tests.

    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    pub struct Reply {
        pub status: u16,
        pub content_type: &'static str,
        pub body: String,
    }

    impl Reply {
        pub fn json(body: &str) -> Self {
            Self {
                status: 200,
                content_type: "application/json",
                body: body.to_string(),
            }
        }

        pub fn with_status(status: u16, body: &str) -> Self {
            Self {
                status,
                content_type: "application/json",
                body: body.to_string(),
            }
        }

        pub fn xml(body: &str) -> Self {
            Self {
                status: 200,
                content_type: "application/rss+xml",
                body: body.to_string(),
            }
        }
    }

    pub struct MockServer {
        pub base: String,
        handle: JoinHandle<Vec<String>>,
    }

    impl MockServer {
        /// Serves `replies` to consecutive connections, then stops.
        pub fn start(replies: Vec<Reply>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
            let base = format!("http://{}", listener.local_addr().expect("local addr"));
            listener.set_nonblocking(true).expect("nonblocking");
            let handle = std::thread::spawn(move || {
                let mut requests = Vec::new();
                let deadline = Instant::now() + Duration::from_secs(20);
                for reply in replies {
                    let stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break Some(stream),
                            Err(_) if Instant::now() < deadline => {
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            Err(_) => break None,
                        }
                    };
                    let Some(mut stream) = stream else { break };
                    stream.set_nonblocking(false).expect("blocking");
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .expect("timeout");
                    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                    let mut request = String::new();
                    let mut content_length = 0usize;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            break;
                        }
                        if let Some((name, value)) = line.split_once(':') {
                            if name.eq_ignore_ascii_case("content-length") {
                                content_length = value.trim().parse().unwrap_or(0);
                            }
                        }
                        let end = line == "\r\n";
                        request.push_str(&line);
                        if end {
                            break;
                        }
                    }
                    let mut body = vec![0u8; content_length];
                    if reader.read_exact(&mut body).is_ok() {
                        request.push_str(&String::from_utf8_lossy(&body));
                    }
                    let reason = if reply.status == 200 { "OK" } else { "Error" };
                    let head = format!(
                        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        reply.status,
                        reply.content_type,
                        reply.body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(reply.body.as_bytes());
                    let _ = stream.flush();
                    requests.push(request);
                }
                requests
            });
            Self { base, handle }
        }

        /// Waits for the server thread and returns the raw requests it saw.
        pub fn requests(self) -> Vec<String> {
            self.handle.join().expect("mock server thread")
        }
    }

    /// The request line (`GET /path?query HTTP/1.1`) of a raw request.
    pub fn request_line(request: &str) -> &str {
        request.lines().next().unwrap_or("")
    }

    pub fn agent() -> ureq::Agent {
        ureq::Agent::new_with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_reserved_characters() {
        let mut s = String::new();
        push_encoded(&mut s, "a b&c=d/é~_.-");
        assert_eq!(s, "a%20b%26c%3Dd%2F%C3%A9~_.-");
    }

    #[test]
    fn query_builder_picks_separators() {
        let mut url = String::from("http://h/p");
        push_query(&mut url, [("a", "1"), ("b", "x y")]);
        assert_eq!(url, "http://h/p?a=1&b=x%20y");
        push_query(&mut url, [("c", "+")]);
        assert_eq!(url, "http://h/p?a=1&b=x%20y&c=%2B");
    }

    #[test]
    fn query_param_round_trips() {
        let mut url = String::from("http://h/rest/stream");
        push_query(&mut url, [("u", "me"), ("id", "al/1 2&3")]);
        assert_eq!(query_param(&url, "id").as_deref(), Some("al/1 2&3"));
        assert_eq!(query_param(&url, "missing"), None);
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz+%41"), "%zz A");
    }

    #[test]
    fn http_url_validation() {
        assert!(is_http_url("https://example.com/feed.xml"));
        assert!(is_http_url("HTTP://example.com"));
        assert!(!is_http_url("ftp://example.com"));
        assert!(!is_http_url("https://user:pw@example.com/"));
        assert!(!is_http_url("https:///nohost"));
        assert!(!is_http_url("file:///etc/passwd"));
    }

    #[test]
    fn random_values_differ() {
        assert_ne!(random_u64(), random_u64());
    }

    #[test]
    fn providers_config_defaults_and_enabled() {
        let cfg: ProvidersConfig = toml::from_str("").expect("empty config");
        assert_eq!(
            cfg.enabled(),
            vec![ProviderKind::Radio, ProviderKind::Podcast]
        );
        let cfg: ProvidersConfig = toml::from_str(
            "[subsonic]\nurl = \"https://music.example\"\nuser = \"me\"\npassword = \"pw\"\n\
             [radio]\nenabled = false\n",
        )
        .expect("config");
        assert_eq!(
            cfg.enabled(),
            vec![ProviderKind::Podcast, ProviderKind::Subsonic]
        );
        let round = toml::to_string(&cfg).expect("serialize");
        let back: ProvidersConfig = toml::from_str(&round).expect("reparse");
        assert_eq!(back, cfg);
    }

    #[test]
    fn status_error_mapping_hides_urls() {
        let err = ProviderError::from(ureq::Error::BadUri("http://h/?t=secret".into()));
        assert!(!err.to_string().contains("secret"));
        assert_eq!(
            ProviderError::from(ureq::Error::StatusCode(404)),
            ProviderError::Status(404)
        );
    }

    #[test]
    fn join_dot_skips_blanks() {
        assert_eq!(
            join_dot([Some("a"), None, Some("  "), Some("b")]).as_deref(),
            Some("a · b")
        );
        assert_eq!(join_dot([None, Some("")]), None);
    }
}
