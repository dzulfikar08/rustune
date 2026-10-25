//! Blocking HTTP via one shared `ureq::Agent` (connection pooling, timeouts).
//! Never call these on the UI thread: wrap in `tokio::task::spawn_blocking`.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::skin_browser::SkinEntry;

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

/// Whether `md5` is a 32-digit hex skin id (safe to use in URLs and paths).
pub fn is_skin_md5(md5: &str) -> bool {
    md5.len() == 32 && md5.bytes().all(|b| b.is_ascii_hexdigit())
}

fn parse_skin_nodes(nodes: &[Value], local: &[String]) -> Vec<SkinEntry> {
    nodes
        .iter()
        .filter(|s| !s["nsfw"].as_bool().unwrap_or(false))
        .filter_map(|s| {
            let md5 = s["md5"].as_str()?.to_ascii_lowercase();
            if !is_skin_md5(&md5) {
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

/// Which museum listing the online browser pages through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkinListing {
    /// Every classic skin, in skins.webamp.org order.
    Museum,
    /// The museum's hand-approved subset.
    Curated,
}

/// The `data.<field>` value of a GraphQL response, or the server's error.
fn graphql_data<'a>(body: &'a Value, field: &str) -> Result<&'a Value> {
    let data = &body["data"][field];
    if data.is_null() {
        if let Some(msg) = body["errors"][0]["message"].as_str() {
            anyhow::bail!("skins.webamp.org: {msg}");
        }
        anyhow::bail!("Unexpected response from skins.webamp.org");
    }
    Ok(data)
}

/// Parse a `skins { nodes count }` page requested at `offset`. NSFW skins are
/// dropped, but paging advances by the server's page so none are skipped.
fn parse_skin_list(
    body: &Value,
    offset: usize,
    limit: usize,
    local: &[String],
) -> Result<SkinPage> {
    let skins = graphql_data(body, "skins")?;
    let nodes = skins["nodes"]
        .as_array()
        .context("Unexpected response from skins.webamp.org")?;
    let total = skins["count"].as_u64().map(|c| c as usize);
    let next_offset = offset + nodes.len();
    let has_more = !nodes.is_empty()
        && match total {
            Some(t) => next_offset < t,
            None => nodes.len() == limit,
        };
    Ok(SkinPage {
        entries: parse_skin_nodes(nodes, local),
        next_offset,
        total,
        has_more,
    })
}

/// Parse a `search_classic_skins` page requested at `offset` (no total).
fn parse_skin_search(
    body: &Value,
    offset: usize,
    limit: usize,
    local: &[String],
) -> Result<SkinPage> {
    let nodes = graphql_data(body, "search_classic_skins")?
        .as_array()
        .context("Unexpected response from skins.webamp.org")?;
    Ok(SkinPage {
        entries: parse_skin_nodes(nodes, local),
        next_offset: offset + nodes.len(),
        total: None,
        has_more: limit > 0 && nodes.len() == limit,
    })
}

const SKIN_FIELDS: &str = "md5 filename nsfw average_color";

/// One page of the museum listing.
pub fn fetch_skin_list(
    listing: SkinListing,
    offset: usize,
    limit: usize,
    skins_dir: &Path,
) -> Result<SkinPage> {
    // The API rejects `sort: MUSEUM` combined with a filter.
    let args = match listing {
        SkinListing::Museum => "sort: MUSEUM",
        SkinListing::Curated => "filter: APPROVED",
    };
    let query = json!({
        "query": format!(
            "query($first: Int!, $offset: Int!) {{ skins(first: $first, offset: $offset, {args}) {{ nodes {{ {SKIN_FIELDS} }} count }} }}"
        ),
        "variables": { "first": limit, "offset": offset }
    });
    let body = post_json(WEBAMP_GRAPHQL, &query)?;
    parse_skin_list(&body, offset, limit, &local_skin_names(skins_dir))
}

/// One page of full-text search over every classic skin, NSFW excluded.
pub fn search_skins(
    query: &str,
    offset: usize,
    limit: usize,
    skins_dir: &Path,
) -> Result<SkinPage> {
    let gql = json!({
        "query": format!(
            "query($q: String!, $first: Int!, $offset: Int!) {{ search_classic_skins(first: $first, offset: $offset, query: $q, nsfw: false) {{ {SKIN_FIELDS} }} }}"
        ),
        "variables": { "q": query, "first": limit, "offset": offset }
    });
    let body = post_json(WEBAMP_GRAPHQL, &gql)?;
    parse_skin_search(&body, offset, limit, &local_skin_names(skins_dir))
}

/// Download the `.wsz` archive of museum skin `md5`.
pub fn fetch_skin_bytes(md5: &str) -> Result<Vec<u8>> {
    anyhow::ensure!(is_skin_md5(md5), "Invalid skin id");
    let bytes = get_bytes(&format!("{SKIN_CDN}/{md5}.wsz"), MAX_SKIN_BYTES)?;
    anyhow::ensure!(
        bytes.starts_with(b"PK"),
        "Downloaded file is not a skin archive"
    );
    Ok(bytes)
}

/// Download a museum skin into `skins_dir` under a sanitised name.
pub fn download_skin(md5: &str, filename: &str, skins_dir: &Path) -> Result<PathBuf> {
    let bytes = fetch_skin_bytes(md5)?;
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

/// Lowercase hex md5 of `bytes` (the museum's skin id).
fn md5_hex(bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    use std::fmt::Write as _;
    let mut out = String::with_capacity(32);
    for b in Md5::digest(bytes) {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// The installed skin whose file content is museum skin `md5`, if any
/// (installed under any name). Reads and hashes the skins one by one.
pub fn installed_skin_with_md5(skins_dir: &Path, md5: &str) -> Option<PathBuf> {
    let md5 = md5.to_ascii_lowercase();
    list_skins(skins_dir).into_iter().find(|path| {
        std::fs::read(path)
            .map(|bytes| md5_hex(&bytes) == md5)
            .unwrap_or(false)
    })
}

/// The museum's file name for skin `md5`.
pub fn fetch_skin_filename(md5: &str) -> Result<String> {
    anyhow::ensure!(is_skin_md5(md5), "Invalid skin id");
    let query = json!({
        "query": "query($md5: String!) { fetch_skin_by_md5(md5: $md5) { filename } }",
        "variables": { "md5": md5 }
    });
    parse_skin_filename(&post_json(WEBAMP_GRAPHQL, &query)?, md5)
}

fn parse_skin_filename(body: &Value, md5: &str) -> Result<String> {
    let skin = graphql_data(body, "fetch_skin_by_md5")
        .map_err(|_| anyhow::anyhow!("No skin {md5} in the Winamp Skin Museum"))?;
    let raw = skin["filename"].as_str().unwrap_or("");
    Ok(sanitize_skin_filename(raw, md5))
}

/// A file name in `skins_dir` for skin `md5` that does not overwrite a
/// different installed skin: `filename`, else `stem-<md5 prefix>.wsz`, else
/// `<md5>.wsz`, else a numbered variant.
pub fn unused_skin_filename(skins_dir: &Path, filename: &str, md5: &str) -> String {
    let filename = sanitize_skin_filename(filename, md5);
    let stem = filename.trim_end_matches(".wsz");
    let short = md5.get(..8).unwrap_or(md5);
    let candidates = [
        filename.clone(),
        format!("{stem}-{short}.wsz"),
        format!("{md5}.wsz"),
    ];
    candidates
        .into_iter()
        .chain((2..).map(|n| format!("{stem}-{short}-{n}.wsz")))
        .find(|name| !skins_dir.join(name).exists())
        .unwrap_or(filename)
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
    fn museum_listing_pages_by_server_count_and_hides_nsfw() {
        let body = json!({"data": {"skins": {
            "count": 103_909,
            "nodes": [
                {"md5": "5e4f10275dcb1fb211d4a8b4f1bda236", "filename": "base-2.91.wsz", "nsfw": false, "average_color": null},
                {"md5": "ffffffffffffffffffffffffffffffff", "filename": "naughty.wsz", "nsfw": true},
                {"md5": "cd251187a5e6ff54ce938d26f1f2de02", "filename": "Winamp3_Classified_v5.5.wsz", "nsfw": false}
            ]
        }}});
        let page = parse_skin_list(&body, 100, 3, &["base-2.91.wsz".into()]).unwrap();
        let names: Vec<&str> = page.entries.iter().map(|e| e.filename.as_str()).collect();
        assert_eq!(names, ["base-2.91.wsz", "Winamp3_Classified_v5.5.wsz"]);
        assert!(page.entries[0].is_local && !page.entries[1].is_local);
        assert_eq!(
            page.next_offset, 103,
            "advances by the server page, not the filtered one"
        );
        assert_eq!(page.total, Some(103_909));
        assert!(page.has_more);

        // A page that is entirely NSFW still moves on.
        let all_nsfw = json!({"data": {"skins": {"count": 10, "nodes": [
            {"md5": "ffffffffffffffffffffffffffffffff", "filename": "a.wsz", "nsfw": true},
            {"md5": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", "filename": "b.wsz", "nsfw": true}
        ]}}});
        let page = parse_skin_list(&all_nsfw, 6, 2, &[]).unwrap();
        assert!(page.entries.is_empty());
        assert_eq!((page.next_offset, page.has_more), (8, true));

        let last = json!({"data": {"skins": {"count": 10, "nodes": [
            {"md5": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", "filename": "b.wsz", "nsfw": false}
        ]}}});
        let page = parse_skin_list(&last, 9, 2, &[]).unwrap();
        assert_eq!((page.next_offset, page.has_more), (10, false));

        let empty = json!({"data": {"skins": {"nodes": []}}});
        assert!(!parse_skin_list(&empty, 0, 2, &[]).unwrap().has_more);
    }

    #[test]
    fn classic_search_parses_a_bare_list() {
        let body = json!({"data": {"search_classic_skins": [
            {"md5": "1f8214678f97b888dfab7c251c943225", "filename": "zelda_theme_by_shermanfirst.zip", "nsfw": false, "average_color": null},
            {"md5": "205d326f05b5569b320d5abf6b6b4043", "filename": "ZeldaXtreme.wsz", "nsfw": false, "average_color": "#102030"}
        ]}});
        let page = parse_skin_search(&body, 0, 2, &[]).unwrap();
        assert_eq!(page.entries[0].filename, "zelda_theme_by_shermanfirst.wsz");
        assert_eq!(page.entries[1].average_color.as_deref(), Some("#102030"));
        assert_eq!(
            (page.next_offset, page.total, page.has_more),
            (2, None, true)
        );
        let short = parse_skin_search(&body, 4, 50, &[]).unwrap();
        assert_eq!((short.next_offset, short.has_more), (6, false));
    }

    #[test]
    fn graphql_errors_are_surfaced() {
        let body = json!({
            "errors": [{"message": "Combining `sort: MUSEUM` with `filter` is not supported."}],
            "data": {"skins": null}
        });
        let err = parse_skin_list(&body, 0, 2, &[])
            .map(|_| ())
            .unwrap_err()
            .to_string();
        assert!(err.contains("not supported"), "{err}");
        assert!(parse_skin_search(&json!({}), 0, 2, &[]).is_err());
    }

    #[test]
    #[ignore = "network"]
    fn fetch_first_page_from_museum() {
        let dir = std::env::temp_dir();
        let page = fetch_skin_list(SkinListing::Museum, 0, 5, &dir).unwrap();
        assert_eq!(page.entries[0].md5, DEFAULT_SKIN_MD5);
        assert!(page.total.unwrap() > 100_000);
        let found = search_skins("zelda", 0, 5, &dir).unwrap();
        assert!(!found.entries.is_empty());
    }

    fn temp_skins(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rt-net-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn installed_skins_are_found_by_content_md5() {
        let dir = temp_skins("md5");
        std::fs::write(dir.join("base-2.91.wsz"), b"abc").unwrap();
        std::fs::write(dir.join("other.wsz"), b"xyz").unwrap();
        // md5("abc"), in either case.
        let md5 = "900150983cd24fb0d6963f7d28e17f72";
        assert_eq!(md5_hex(b"abc"), md5);
        assert_eq!(
            installed_skin_with_md5(&dir, &md5.to_uppercase()),
            Some(dir.join("base-2.91.wsz"))
        );
        assert_eq!(installed_skin_with_md5(&dir, MD5), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn new_skins_never_overwrite_a_different_one() {
        let dir = temp_skins("unused");
        assert_eq!(unused_skin_filename(&dir, "Cool.wsz", MD5), "Cool.wsz");
        std::fs::write(dir.join("Cool.wsz"), b"x").unwrap();
        assert_eq!(
            unused_skin_filename(&dir, "Cool.wsz", MD5),
            "Cool-01234567.wsz"
        );
        std::fs::write(dir.join("Cool-01234567.wsz"), b"x").unwrap();
        assert_eq!(
            unused_skin_filename(&dir, "Cool.wsz", MD5),
            format!("{MD5}.wsz")
        );
        std::fs::write(dir.join(format!("{MD5}.wsz")), b"x").unwrap();
        assert_eq!(
            unused_skin_filename(&dir, "Cool.wsz", MD5),
            "Cool-01234567-2.wsz"
        );
        assert_eq!(unused_skin_filename(&dir, "../x", MD5), "x.wsz");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn museum_filename_comes_from_graphql() {
        let body = json!({"data": {"fetch_skin_by_md5": {"filename": "base-2.91.wsz"}}});
        assert_eq!(parse_skin_filename(&body, MD5).unwrap(), "base-2.91.wsz");
        let body = json!({"data": {"fetch_skin_by_md5": {"filename": "../Evil.zip"}}});
        assert_eq!(parse_skin_filename(&body, MD5).unwrap(), "Evil.wsz");
        let missing = json!({"data": {"fetch_skin_by_md5": null}});
        assert!(parse_skin_filename(&missing, MD5)
            .unwrap_err()
            .to_string()
            .contains("No skin"));
    }
}
