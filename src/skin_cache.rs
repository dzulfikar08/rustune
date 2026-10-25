//! Size-capped LRU cache of museum skins downloaded for the skin browser's
//! live preview. Installing a previewed skin moves its cached archive into
//! the skins directory instead of downloading it again. Blocking IO: call
//! from `spawn_blocking`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};

use crate::net::{is_skin_md5, sanitize_skin_filename};

/// Default size cap of the preview cache.
pub const PREVIEW_CACHE_CAP: u64 = 30 * 1024 * 1024;

pub struct PreviewCache {
    dir: PathBuf,
    cap: u64,
}

impl PreviewCache {
    pub fn new(dir: PathBuf, cap: u64) -> Self {
        Self { dir, cap }
    }

    /// `<cache dir>/rustune/skin-preview`, capped at [`PREVIEW_CACHE_CAP`].
    pub fn user() -> Self {
        let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
        Self::new(base.join("rustune").join("skin-preview"), PREVIEW_CACHE_CAP)
    }

    fn path(&self, md5: &str) -> Result<PathBuf> {
        anyhow::ensure!(is_skin_md5(md5), "Invalid skin id");
        Ok(self.dir.join(format!("{}.wsz", md5.to_ascii_lowercase())))
    }

    /// The archive of skin `md5`: from the cache (marking it recently used)
    /// or from `fetch`, which is then cached and the cache trimmed.
    pub fn load(&self, md5: &str, fetch: impl FnOnce(&str) -> Result<Vec<u8>>) -> Result<Vec<u8>> {
        let path = self.path(md5)?;
        if let Ok(bytes) = fs::read(&path) {
            let _ = touch(&path);
            return Ok(bytes);
        }
        let bytes = fetch(md5)?;
        // A cache that cannot be written only costs a later re-download.
        if crate::config::write_atomic(&path, &bytes).is_ok() {
            let _ = self.evict(Some(&path));
        }
        Ok(bytes)
    }

    /// Delete least recently used archives until the cache fits its cap.
    /// `keep` (the archive just added) is never deleted.
    pub fn evict(&self, keep: Option<&Path>) -> io::Result<()> {
        let mut files: Vec<(SystemTime, PathBuf, u64)> = fs::read_dir(&self.dir)?
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                let is_wsz = path
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("wsz"));
                let meta = e.metadata().ok().filter(|m| m.is_file() && is_wsz)?;
                let used = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                Some((used, path, meta.len()))
            })
            .collect();
        let mut total: u64 = files.iter().map(|f| f.2).sum();
        files.sort();
        for (_, path, len) in files {
            if total <= self.cap {
                break;
            }
            if Some(path.as_path()) == keep {
                continue;
            }
            if fs::remove_file(&path).is_ok() {
                total -= len;
            }
        }
        Ok(())
    }

    /// Install skin `md5` into `skins_dir` as the sanitised `filename`,
    /// moving the cached archive there (downloading with `fetch` when it is
    /// not cached).
    pub fn install(
        &self,
        md5: &str,
        filename: &str,
        skins_dir: &Path,
        fetch: impl FnOnce(&str) -> Result<Vec<u8>>,
    ) -> Result<PathBuf> {
        let cached = self.path(md5)?;
        let dest = skins_dir.join(sanitize_skin_filename(filename, md5));
        fs::create_dir_all(skins_dir)
            .with_context(|| format!("Creating {}", skins_dir.display()))?;
        if cached.is_file() && move_file(&cached, &dest).is_ok() {
            return Ok(dest);
        }
        let bytes = fetch(md5)?;
        crate::config::write_atomic(&dest, &bytes)
            .with_context(|| format!("Saving {}", dest.display()))?;
        Ok(dest)
    }
}

/// Mark `path` as recently used.
fn touch(path: &Path) -> io::Result<()> {
    fs::File::options()
        .write(true)
        .open(path)?
        .set_modified(SystemTime::now())
}

/// Rename, or copy and delete when `from` and `to` are on different
/// filesystems.
fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    let mut tmp = to.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::copy(from, &tmp)?;
    fs::rename(&tmp, to)?;
    let _ = fs::remove_file(from);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "rustune-skincache-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn md5(c: char) -> String {
        c.to_string().repeat(32)
    }

    /// Cache file for `id` written `age_s` seconds before a fixed epoch.
    fn put(dir: &Path, id: &str, len: usize, age_s: u64) -> PathBuf {
        let p = dir.join(format!("{id}.wsz"));
        fs::write(&p, vec![b'P'; len]).unwrap();
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - age_s);
        fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(t)
            .unwrap();
        p
    }

    #[test]
    fn eviction_drops_least_recently_used_first() {
        let tmp = TempDir::new("evict");
        let cache = PreviewCache::new(tmp.0.clone(), 250);
        let oldest = put(&tmp.0, &md5('a'), 100, 30);
        let middle = put(&tmp.0, &md5('b'), 100, 20);
        let newest = put(&tmp.0, &md5('c'), 100, 10);
        let other = tmp.0.join("notes.txt");
        fs::write(&other, vec![0; 1000]).unwrap();
        cache.evict(None).unwrap();
        assert!(!oldest.exists(), "oldest goes first");
        assert!(middle.exists() && newest.exists());
        assert!(other.exists(), "only skin archives are managed");

        // The archive just added survives even when it alone is over the cap.
        let tiny = PreviewCache::new(tmp.0.clone(), 50);
        tiny.evict(Some(&middle)).unwrap();
        assert!(middle.exists() && !newest.exists());
    }

    #[test]
    fn hits_skip_the_fetch_and_refresh_recency() {
        let tmp = TempDir::new("hit");
        let cache = PreviewCache::new(tmp.0.clone(), 250);
        let a = put(&tmp.0, &md5('a'), 100, 30);
        put(&tmp.0, &md5('b'), 100, 20);
        let bytes = cache
            .load(&md5('a'), |_| panic!("cached skins are not downloaded"))
            .unwrap();
        assert_eq!(bytes.len(), 100);
        // `a` is now the most recent, so adding `c` evicts `b`.
        let c = cache.load(&md5('c'), |_| Ok(vec![b'P'; 100])).unwrap();
        assert_eq!(c.len(), 100);
        assert!(a.exists());
        assert!(!tmp.0.join(format!("{}.wsz", md5('b'))).exists());
        assert!(tmp.0.join(format!("{}.wsz", md5('c'))).exists());
        assert!(cache.load("../../etc/passwd", |_| Ok(Vec::new())).is_err());
    }

    #[test]
    fn install_moves_the_cached_archive() {
        let tmp = TempDir::new("install");
        let cache_dir = tmp.0.join("cache");
        let skins = tmp.0.join("skins");
        fs::create_dir_all(&cache_dir).unwrap();
        let cache = PreviewCache::new(cache_dir.clone(), PREVIEW_CACHE_CAP);
        let cached = put(&cache_dir, &md5('d'), 10, 0);
        let path = cache
            .install(&md5('d'), "../Cool Skin.zip", &skins, |_| {
                panic!("cached skins are not downloaded")
            })
            .unwrap();
        assert_eq!(path, skins.join("Cool Skin.wsz"));
        assert_eq!(fs::read(&path).unwrap().len(), 10);
        assert!(!cached.exists(), "moved, not copied");

        let fetched = cache
            .install(&md5('e'), "Other.wsz", &skins, |_| Ok(b"PK..".to_vec()))
            .unwrap();
        assert_eq!(fs::read(fetched).unwrap(), b"PK..");
    }
}
