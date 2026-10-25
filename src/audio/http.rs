//! Native HTTP sources (blocking, used only on decoder threads):
//! range-seekable resources, live streams (with ICY metadata) and radio
//! playlists (.pls/.m3u, resolved to their first entry; HLS is reported so the
//! caller can hand it to ffmpeg).

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{bail, Context};
use symphonia::core::io::MediaSource;
use ureq::{Agent, BodyReader};

/// Forward seeks up to this many bytes read through instead of reconnecting.
const SKIP_READ_MAX: u64 = 128 * 1024;
const PLAYLIST_MAX: u64 = 64 * 1024;

static AGENT: LazyLock<Agent> = LazyLock::new(|| {
    Agent::new_with_config(
        Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(15)))
            .timeout_per_read(Some(Duration::from_secs(20)))
            // Byte offsets must match the resource for Range requests.
            .accept_encoding("identity")
            .user_agent(concat!("rustune/", env!("CARGO_PKG_VERSION")))
            .build(),
    )
});

fn io_err(e: ureq::Error) -> io::Error {
    match e {
        ureq::Error::Io(e) => e,
        e => io::Error::other(e.to_string()),
    }
}

/// `Read` adaptor that is `Sync` without locking: the mutex is only ever
/// accessed through `get_mut` (exclusive access), as `MediaSource` requires `Sync`.
pub struct SyncRead<R>(Mutex<R>);

impl<R: Read> SyncRead<R> {
    pub fn new(r: R) -> Self {
        Self(Mutex::new(r))
    }
}

impl<R: Read> Read for SyncRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .read(buf)
    }
}

/// Non-seekable `MediaSource` over any reader (live streams).
pub struct LiveSource<R>(SyncRead<R>);

impl<R: Read + Send> LiveSource<R> {
    pub fn new(r: R) -> Self {
        Self(SyncRead::new(r))
    }
}

impl<R: Read> Read for LiveSource<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl<R: Read> Seek for LiveSource<R> {
    fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "live stream is not seekable",
        ))
    }
}

impl<R: Read + Send> MediaSource for LiveSource<R> {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

/// Seekable HTTP resource read through `Range` requests, reconnecting once on
/// a dropped connection.
pub struct RangeReader {
    url: String,
    len: u64,
    pos: u64,
    body: Option<SyncRead<BodyReader<'static>>>,
    /// Connections opened (for tests and diagnostics).
    connects: u32,
}

impl RangeReader {
    /// Wrap an already-open response body positioned at offset 0.
    fn with_body(url: String, len: u64, body: BodyReader<'static>) -> Self {
        Self {
            url,
            len,
            pos: 0,
            body: Some(SyncRead::new(body)),
            connects: 1,
        }
    }

    /// Open `url` at offset 0 and verify the server honours ranges.
    pub fn open(url: &str) -> io::Result<Self> {
        let mut r = Self {
            url: url.to_owned(),
            len: 0,
            pos: 0,
            body: None,
            connects: 0,
        };
        r.connect()?;
        Ok(r)
    }

    #[cfg(test)]
    pub fn connects(&self) -> u32 {
        self.connects
    }

    fn connect(&mut self) -> io::Result<()> {
        self.body = None;
        let resp = AGENT
            .get(&self.url)
            .header("Range", format!("bytes={}-", self.pos))
            .call()
            .map_err(io_err)?;
        self.connects += 1;
        let headers = resp.headers();
        match resp.status().as_u16() {
            206 => {
                if let Some(total) =
                    header(headers, "content-range").and_then(parse_content_range_total)
                {
                    self.len = total;
                }
            }
            200 if self.pos == 0 => {
                if let Some(len) = header(headers, "content-length").and_then(|v| v.parse().ok()) {
                    self.len = len;
                }
            }
            code => {
                return Err(io::Error::other(format!(
                    "server ignored range request (HTTP {code})"
                )))
            }
        }
        self.body = Some(SyncRead::new(resp.into_body().into_reader()));
        Ok(())
    }
}

impl Read for RangeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.len {
            return Ok(0);
        }
        let mut retried = false;
        loop {
            let body = match self.body.as_mut() {
                Some(b) => b,
                None => {
                    self.connect()?;
                    continue;
                }
            };
            match body.read(buf) {
                Ok(0) if !retried => {
                    // Premature end (connection dropped): resume at `pos`.
                    retried = true;
                    self.body = None;
                }
                Ok(n) => {
                    self.pos += n as u64;
                    return Ok(n);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) if !retried => {
                    retried = true;
                    self.body = None;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl Seek for RangeReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(d) => i128::from(self.pos) + i128::from(d),
            SeekFrom::End(d) => i128::from(self.len) + i128::from(d),
        };
        let target = u64::try_from(target)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?;
        if target == self.pos {
            return Ok(target);
        }
        if target > self.pos && target - self.pos <= SKIP_READ_MAX && self.body.is_some() {
            let skip = target - self.pos;
            let skipped = match self.body.as_mut() {
                Some(body) => io::copy(&mut body.take(skip), &mut io::sink()).unwrap_or(0),
                None => 0,
            };
            self.pos += skipped;
            if skipped == skip {
                return Ok(target);
            }
        }
        self.body = None;
        self.pos = target;
        Ok(target)
    }
}

impl MediaSource for RangeReader {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

/// `Content-Range: bytes 0-99/1234` → 1234.
fn parse_content_range_total(v: &str) -> Option<u64> {
    v.rsplit('/').next()?.trim().parse().ok()
}

fn header<'a>(h: &'a ureq::http::HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok()).map(str::trim)
}

/// Result of opening a URL.
pub enum Opened {
    Seekable(RangeReader),
    /// Non-seekable body. `icy_metaint` is set when the server interleaves ICY metadata.
    Stream {
        body: BodyReader<'static>,
        icy_metaint: Option<usize>,
    },
    /// HTTP Live Streaming playlist: needs ffmpeg.
    Hls,
}

pub struct HttpSource {
    /// URL actually opened (after playlist resolution).
    pub url: String,
    pub mime: Option<String>,
    /// From `icy-br` (radio).
    pub kbps: Option<u32>,
    pub opened: Opened,
}

/// Open `url`, following radio playlists (.pls/.m3u) up to three levels deep.
pub fn open(url: &str) -> anyhow::Result<HttpSource> {
    let mut url = url.to_owned();
    for _ in 0..3 {
        let resp = AGENT
            .get(&url)
            .header("Icy-MetaData", "1")
            .call()
            .with_context(|| format!("HTTP request failed: {url}"))?;
        let headers = resp.headers();
        let mime = header(headers, "content-type")
            .map(|v| v.split(';').next().unwrap_or(v).trim().to_ascii_lowercase());
        let icy_metaint = header(headers, "icy-metaint")
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0);
        let kbps = header(headers, "icy-br").and_then(|v| v.split(',').next()?.trim().parse().ok());
        let len = header(headers, "content-length").and_then(|v| v.parse::<u64>().ok());
        let ranges =
            header(headers, "accept-ranges").is_some_and(|v| v.eq_ignore_ascii_case("bytes"));

        if icy_metaint.is_none() && is_playlist(mime.as_deref(), &url) {
            let mut text = String::new();
            resp.into_body()
                .into_reader()
                .take(PLAYLIST_MAX)
                .read_to_string(&mut text)
                .context("failed to read playlist")?;
            if text.contains("#EXT-X-") {
                return Ok(HttpSource {
                    url,
                    mime,
                    kbps,
                    opened: Opened::Hls,
                });
            }
            match first_playlist_entry(&text, &url) {
                Some(next) => {
                    url = next;
                    continue;
                }
                None => bail!("playlist has no playable entries: {url}"),
            }
        }

        let body = resp.into_body().into_reader();
        let opened = match (icy_metaint, ranges, len) {
            (None, true, Some(len)) if len > 0 => {
                Opened::Seekable(RangeReader::with_body(url.clone(), len, body))
            }
            _ => Opened::Stream { body, icy_metaint },
        };
        return Ok(HttpSource {
            url,
            mime,
            kbps,
            opened,
        });
    }
    bail!("too many nested playlists: {url}")
}

/// Lower-cased path extension of a URL (query/fragment stripped).
pub fn url_extension(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let last = path.rsplit('/').next().unwrap_or(path);
    let (_, ext) = last.rsplit_once('.')?;
    (!ext.is_empty() && ext.len() <= 5).then(|| ext.to_ascii_lowercase())
}

fn is_playlist(mime: Option<&str>, url: &str) -> bool {
    let by_mime = matches!(
        mime,
        Some(
            "audio/x-scpls"
                | "audio/scpls"
                | "application/pls+xml"
                | "audio/x-mpegurl"
                | "audio/mpegurl"
                | "application/x-mpegurl"
                | "application/mpegurl"
                | "application/vnd.apple.mpegurl"
        )
    );
    by_mime || matches!(url_extension(url).as_deref(), Some("pls" | "m3u" | "m3u8"))
}

/// First stream URL in a PLS or M3U body, resolved against `base`.
pub fn first_playlist_entry(text: &str, base: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let entry = match line.split_once('=') {
            // PLS: "File1=http://..."; other keys (Title1=, Length1=) are skipped.
            Some((key, value)) if key.trim().to_ascii_lowercase().starts_with("file") => {
                value.trim()
            }
            Some(_) if !line.contains("://") => continue,
            _ => line,
        };
        if let Some(url) = resolve(entry, base) {
            return Some(url);
        }
    }
    None
}

fn resolve(entry: &str, base: &str) -> Option<String> {
    let lower = entry.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(entry.to_owned());
    }
    if entry.contains("://") {
        return None;
    }
    let scheme_end = base.find("://")? + 3;
    let host_end = base[scheme_end..]
        .find('/')
        .map_or(base.len(), |i| scheme_end + i);
    if let Some(abs) = entry.strip_prefix('/') {
        return Some(format!("{}/{}", &base[..host_end], abs));
    }
    let path = base.split(['?', '#']).next().unwrap_or(base);
    let dir_end = path
        .rfind('/')
        .filter(|&i| i >= host_end)
        .unwrap_or(path.len());
    Some(format!("{}/{}", &path[..dir_end], entry))
}

#[cfg(test)]
pub(crate) mod test_server {
    //! Minimal in-process HTTP/1.1 server for tests (no network access).

    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    pub struct Route {
        pub path: &'static str,
        pub body: Vec<u8>,
        pub content_type: &'static str,
        pub ranges: bool,
        pub extra_headers: Vec<(&'static str, String)>,
    }

    pub struct Server {
        pub base: String,
        pub requests: Arc<AtomicUsize>,
    }

    pub fn serve(routes: Vec<Route>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        let routes = Arc::new(routes);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                counter.fetch_add(1, Ordering::SeqCst);
                let routes = routes.clone();
                std::thread::spawn(move || handle(stream, &routes));
            }
        });
        Server { base, requests }
    }

    fn handle(mut s: TcpStream, routes: &[Route]) {
        let mut req = Vec::new();
        let mut b = [0u8; 1];
        while !req.ends_with(b"\r\n\r\n") {
            match s.read(&mut b) {
                Ok(1) => req.push(b[0]),
                _ => return,
            }
        }
        let req = String::from_utf8_lossy(&req).to_string();
        let path = req.split_whitespace().nth(1).unwrap_or("/").to_owned();
        let range_start = req.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if !k.trim().eq_ignore_ascii_case("range") {
                return None;
            }
            v.trim()
                .strip_prefix("bytes=")?
                .trim_end_matches('-')
                .parse::<usize>()
                .ok()
        });
        let Some(route) = routes.iter().find(|r| r.path == path) else {
            let _ = s.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return;
        };
        let mut head = String::new();
        let body: &[u8] = match (route.ranges, range_start) {
            (true, Some(start)) if start < route.body.len() => {
                head.push_str("HTTP/1.1 206 Partial Content\r\n");
                head.push_str(&format!(
                    "Content-Range: bytes {}-{}/{}\r\n",
                    start,
                    route.body.len() - 1,
                    route.body.len()
                ));
                &route.body[start..]
            }
            _ => {
                head.push_str("HTTP/1.1 200 OK\r\n");
                &route.body
            }
        };
        if route.ranges {
            head.push_str("Accept-Ranges: bytes\r\n");
        }
        head.push_str(&format!("Content-Type: {}\r\n", route.content_type));
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
        for (k, v) in &route.extra_headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("Connection: close\r\n\r\n");
        let _ = s.write_all(head.as_bytes());
        let _ = s.write_all(body);
    }
}

#[cfg(test)]
mod tests {
    use super::test_server::{serve, Route};
    use super::*;
    use std::sync::atomic::Ordering;

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 % 251) as u8).collect()
    }

    fn route(path: &'static str, body: Vec<u8>, ranges: bool) -> Route {
        Route {
            path,
            body,
            content_type: "audio/mpeg",
            ranges,
            extra_headers: Vec::new(),
        }
    }

    #[test]
    fn range_reader_reads_and_seeks() {
        let body = data(600_000);
        let srv = serve(vec![route("/a.mp3", body.clone(), true)]);
        let mut r = RangeReader::open(&format!("{}/a.mp3", srv.base)).unwrap();
        assert_eq!(r.byte_len(), Some(600_000));
        assert!(r.is_seekable());

        let mut head = [0u8; 1000];
        r.read_exact(&mut head).unwrap();
        assert_eq!(&head[..], &body[..1000]);

        // Small forward seek reads through on the same connection.
        assert_eq!(r.seek(SeekFrom::Current(5_000)).unwrap(), 6_000);
        let mut b = [0u8; 10];
        r.read_exact(&mut b).unwrap();
        assert_eq!(&b[..], &body[6_000..6_010]);
        assert_eq!(r.connects(), 1);

        // Large and backward seeks reconnect with a Range header.
        r.seek(SeekFrom::Start(500_000)).unwrap();
        r.read_exact(&mut b).unwrap();
        assert_eq!(&b[..], &body[500_000..500_010]);
        r.seek(SeekFrom::Start(10)).unwrap();
        r.read_exact(&mut b).unwrap();
        assert_eq!(&b[..], &body[10..20]);
        assert_eq!(r.connects(), 3);

        r.seek(SeekFrom::End(-4)).unwrap();
        let mut tail = Vec::new();
        r.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, &body[body.len() - 4..]);
        assert!(r.seek(SeekFrom::Current(-1_000_000)).is_err());

        // Seeking past the end reads nothing.
        r.seek(SeekFrom::Start(700_000)).unwrap();
        assert_eq!(r.read(&mut b).unwrap(), 0);
    }

    #[test]
    fn range_reader_rejects_server_without_ranges() {
        let srv = serve(vec![route("/n.mp3", data(400_000), false)]);
        let mut r = RangeReader::open(&format!("{}/n.mp3", srv.base)).unwrap();
        // Too far to read through: needs a Range request, which the server ignores.
        r.seek(SeekFrom::Start(300_000)).unwrap();
        assert!(r.read(&mut [0u8; 4]).is_err());
    }

    #[test]
    fn open_classifies_responses() {
        let mut icy = route("/live", data(5000), false);
        icy.extra_headers = vec![("icy-metaint", "16000".into()), ("icy-br", "128".into())];
        let srv = serve(vec![
            route("/song.mp3", data(5000), true),
            route("/stream", data(5000), false),
            icy,
            Route {
                path: "/radio.pls",
                body: b"[playlist]\nNumberOfEntries=1\nTitle1=x\nFile1=/live\n".to_vec(),
                content_type: "audio/x-scpls",
                ranges: false,
                extra_headers: Vec::new(),
            },
            Route {
                path: "/hls.m3u8",
                body: b"#EXTM3U\n#EXT-X-TARGETDURATION:10\nseg1.ts\n".to_vec(),
                content_type: "application/vnd.apple.mpegurl",
                ranges: false,
                extra_headers: Vec::new(),
            },
        ]);
        let b = &srv.base;

        let s = open(&format!("{b}/song.mp3")).unwrap();
        assert!(matches!(s.opened, Opened::Seekable(_)));
        assert_eq!(s.mime.as_deref(), Some("audio/mpeg"));

        let s = open(&format!("{b}/stream")).unwrap();
        assert!(matches!(
            s.opened,
            Opened::Stream {
                icy_metaint: None,
                ..
            }
        ));

        let s = open(&format!("{b}/radio.pls")).unwrap();
        assert_eq!(s.url, format!("{b}/live"));
        assert_eq!(s.kbps, Some(128));
        assert!(matches!(
            s.opened,
            Opened::Stream {
                icy_metaint: Some(16000),
                ..
            }
        ));

        let s = open(&format!("{b}/hls.m3u8")).unwrap();
        assert!(matches!(s.opened, Opened::Hls));

        assert!(open(&format!("{b}/missing")).is_err());
        assert!(srv.requests.load(Ordering::SeqCst) >= 6);
    }

    #[test]
    fn playlist_parsing_and_url_resolution() {
        let pls = "[playlist]\r\nNumberOfEntries=2\r\nFile1=http://a.example/s1\r\nTitle1=One\r\nFile2=http://b/s2\r\n";
        assert_eq!(
            first_playlist_entry(pls, "http://x/p.pls").as_deref(),
            Some("http://a.example/s1")
        );
        let m3u = "#EXTM3U\n#EXTINF:-1,Radio\nstream.mp3\n";
        assert_eq!(
            first_playlist_entry(m3u, "https://host:8000/dir/list.m3u?x=1").as_deref(),
            Some("https://host:8000/dir/stream.mp3")
        );
        assert_eq!(
            first_playlist_entry("/abs/s\n", "http://h/dir/l.m3u").as_deref(),
            Some("http://h/abs/s")
        );
        assert_eq!(first_playlist_entry("rtsp://nope\n", "http://h/"), None);
        assert_eq!(
            url_extension("http://h/a/B.MP3?x=1#f").as_deref(),
            Some("mp3")
        );
        assert_eq!(url_extension("http://h/stream"), None);
        assert_eq!(parse_content_range_total("bytes 0-99/1234"), Some(1234));
    }
}
