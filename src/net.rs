//! Blocking HTTP via one shared `ureq::Agent` (connection pooling, timeouts).
//! Never call these on the UI thread: wrap in `tokio::task::spawn_blocking`.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::app::SkinEntry;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const READ_TIMEOUT: Duration = Duration::from_secs(20);
/// Upper bound for one whole request (GraphQL page or skin download).
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Skins are a few hundred KiB; refuse anything absurd.
const MAX_SKIN_BYTES: u64 = 16 * 1024 * 1024;

const WEBAMP_GRAPHQL: &str = "https://skins.webamp.org/graphql";
const SKIN_CDN: &str = "https://r2.webampskins.org/skins";
/// Winamp 2.91 base skin on the webamp museum.
pub const DEFAULT_SKIN_MD5: &str = "5e4f10275dcb1fb211d4a8b4f1bda236";
pub const DEFAULT_SKIN_FILE: &str = "base-2.91.wsz";

static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(READ_TIMEOUT))
        .timeout_recv_body(Some(CALL_TIMEOUT))
        .timeout_global(Some(CALL_TIMEOUT))
        .user_agent(concat!("rustune/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
});

/// The process-wide HTTP agent.
pub fn agent() -> &'static ureq::Agent {
    &AGENT
}

pub fn get_bytes(url: &str, limit: u64) -> Result<Vec<u8>> {
    let mut resp = agent()
        .get(url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .with_context(|| format!("Reading {url}"))?;
    Ok(bytes)
}

pub fn post_json(url: &str, body: &Value) -> Result<Value> {
    let mut resp = agent()
        .post(url)
        .send_json(body)
        .with_context(|| format!("POST {url}"))?;
    let value = resp
        .body_mut()
        .read_json::<Value>()
        .with_context(|| format!("Decoding response from {url}"))?;
    Ok(value)
}

/// A page of webamp museum skins.
pub struct SkinPage {
    pub entries: Vec<SkinEntry>,
    /// Offset to request next (advances by the server page, before NSFW filtering).
    pub next_offset: usize,
    /// Total available, when the API reports it.
    pub total: Option<usize>,
    pub has_more: bool,
}

/// Make an API-provided skin filename safe to join onto the skins directory:
/// no separators or traversal, no control characters, `.wsz` extension.
pub fn sanitize_skin_filename(raw: &str, md5: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut name: String = base
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.()[]+&,!'".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".zip") || lower.ends_with(".wsz") {
        name.truncate(name.len() - 4);
    }
    let stem: String = name
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .chars()
        .take(120)
        .collect();
    let stem = if stem.is_empty() {
        md5.chars().filter(char::is_ascii_hexdigit).collect()
    } else {
        stem
    };
    let stem = if stem.is_empty() {
        "skin".to_string()
    } else {
        stem
    };
    format!("{stem}.wsz")
}

fn parse_skin_nodes(nodes: &[Value], local: &[String]) -> Vec<SkinEntry> {
    nodes
        .iter()
        .filter(|s| !s["nsfw"].as_bool().unwrap_or(false))
        .filter_map(|s| {
            let md5 = s["md5"].as_str()?.to_ascii_lowercase();
            if md5.len() != 32 || !md5.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            let raw = s["filename"].as_str().unwrap_or("");
            let filename = sanitize_skin_filename(raw, &md5);
            let display_name = filename.trim_end_matches(".wsz").to_string();
            let is_local = local.iter().any(|l| l.eq_ignore_ascii_case(&filename));
            Some(SkinEntry {
                md5,
                is_local,
                filename,
                display_name,
                average_color: s["average_color"].as_str().map(String::from),
            })
        })
        .collect()
}

/// File names of the installed `.wsz` skins.
pub fn local_skin_names(dir: &Path) -> Vec<String> {
    list_skins(dir)
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(String::from))
        .collect()
}

/// Installed `.wsz` skins, sorted. A missing/unreadable directory is empty.
pub fn list_skins(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut skins: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("wsz"))
        })
        .collect();
    skins.sort();
    skins
}

/// One page of the approved skin list.
pub fn fetch_skin_list(offset: usize, limit: usize, skins_dir: &Path) -> Result<SkinPage> {
    let query = json!({
        "query": "query($first: Int!, $offset: Int!) { skins(first: $first, offset: $offset, filter: APPROVED) { nodes { md5 filename nsfw average_color } count } }",
        "variables": { "first": limit, "offset": offset }
    });
    let body = post_json(WEBAMP_GRAPHQL, &query)?;
    let skins = &body["data"]["skins"];
    let nodes = skins["nodes"]
        .as_array()
        .context("Unexpected response from skins.webamp.org")?;
    let total = skins["count"].as_u64().map(|c| c as usize);
    let next_offset = offset + nodes.len();
    let has_more = match total {
        Some(t) => next_offset < t,
        None => nodes.len() == limit,
    };
    Ok(SkinPage {
        entries: parse_skin_nodes(nodes, &local_skin_names(skins_dir)),
        next_offset,
        total,
        has_more,
    })
}

/// One page of full-text skin search results.
pub fn search_skins(
    query: &str,
    offset: usize,
    limit: usize,
    skins_dir: &Path,
) -> Result<SkinPage> {
    let gql = json!({
        "query": "query($q: String!, $first: Int!, $offset: Int!) { search_skins(first: $first, offset: $offset, query: $q) { md5 filename nsfw average_color } }",
        "variables": { "q": query, "first": limit, "offset": offset }
    });
    let body = post_json(WEBAMP_GRAPHQL, &gql)?;
    let nodes = body["data"]["search_skins"]
        .as_array()
        .context("Unexpected response from skins.webamp.org")?;
    Ok(SkinPage {
        entries: parse_skin_nodes(nodes, &local_skin_names(skins_dir)),
        next_offset: offset + nodes.len(),
        total: None,
        has_more: nodes.len() == limit,
    })
}

/// Download a museum skin into `skins_dir` under a sanitised name.
pub fn download_skin(md5: &str, filename: &str, skins_dir: &Path) -> Result<PathBuf> {
    anyhow::ensure!(
        md5.len() == 32 && md5.chars().all(|c| c.is_ascii_hexdigit()),
        "Invalid skin id"
    );
    let bytes = get_bytes(&format!("{SKIN_CDN}/{md5}.wsz"), MAX_SKIN_BYTES)?;
    anyhow::ensure!(
        bytes.starts_with(b"PK"),
        "Downloaded file is not a skin archive"
    );
    let path = skins_dir.join(sanitize_skin_filename(filename, md5));
    crate::config::write_atomic(&path, &bytes)
        .with_context(|| format!("Saving {}", path.display()))?;
    Ok(path)
}

/// Fetch the base Winamp skin if it is not installed yet. Returns the path
/// when it was downloaded now.
pub fn ensure_default_skin(skins_dir: &Path) -> Result<Option<PathBuf>> {
    if skins_dir.join(DEFAULT_SKIN_FILE).exists() {
        return Ok(None);
    }
    download_skin(DEFAULT_SKIN_MD5, DEFAULT_SKIN_FILE, skins_dir).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MD5: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn sanitize_blocks_traversal_and_separators() {
        assert_eq!(sanitize_skin_filename("../../.bashrc", MD5), "bashrc.wsz");
        assert_eq!(sanitize_skin_filename("..\\..\\evil.wsz", MD5), "evil.wsz");
        assert_eq!(sanitize_skin_filename("/etc/passwd", MD5), "passwd.wsz");
        assert_eq!(
            sanitize_skin_filename("dir/Cool Skin.WSZ", MD5),
            "Cool Skin.wsz"
        );
        assert_eq!(sanitize_skin_filename("a\u{0}b\nc.zip", MD5), "a_b_c.wsz");
        assert_eq!(sanitize_skin_filename("..", MD5), format!("{MD5}.wsz"));
        assert_eq!(sanitize_skin_filename("", "zz"), "skin.wsz");
        let long = "x".repeat(500);
        assert_eq!(sanitize_skin_filename(&long, MD5).len(), 124);
    }

    #[test]
    fn sanitized_names_stay_inside_the_directory() {
        let dir = Path::new("/skins");
        for raw in ["../x", "..", "/abs", "a/../../b", "C:\\x\\y.wsz", "...wsz"] {
            let p = dir.join(sanitize_skin_filename(raw, MD5));
            assert_eq!(p.parent(), Some(dir), "{raw}");
            assert!(!p.file_name().unwrap().to_string_lossy().starts_with('.'));
        }
    }

    #[test]
    fn skin_nodes_filter_nsfw_and_mark_local() {
        let nodes = vec![
            json!({"md5": MD5.to_uppercase(), "filename": "Base 2.91.wsz", "nsfw": false, "average_color": "rgb(1,2,3)"}),
            json!({"md5": "ffffffffffffffffffffffffffffffff", "filename": "naughty.wsz", "nsfw": true}),
            json!({"md5": "not-an-md5", "filename": "bad.wsz"}),
            json!({"md5": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", "filename": "../Other.zip"}),
        ];
        let entries = parse_skin_nodes(&nodes, &["base 2.91.WSZ".to_string()]);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].md5, MD5);
        assert_eq!(entries[0].filename, "Base 2.91.wsz");
        assert_eq!(entries[0].display_name, "Base 2.91");
        assert!(entries[0].is_local);
        assert_eq!(entries[0].average_color.as_deref(), Some("rgb(1,2,3)"));
        assert_eq!(entries[1].filename, "Other.wsz");
        assert!(!entries[1].is_local);
    }

    #[test]
    fn list_skins_tolerates_missing_dir() {
        assert!(list_skins(Path::new("/definitely/missing/skins")).is_empty());
    }

    #[test]
    #[ignore = "network"]
    fn fetch_first_page_from_museum() {
        let dir = std::env::temp_dir();
        let page = fetch_skin_list(0, 5, &dir).unwrap();
        assert!(!page.entries.is_empty());
        assert!(page.next_offset > 0);
    }
}
