use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::audio::ReplayGainMode;
use crate::providers::ProvidersConfig;

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    #[serde(default = "default_music_dir")]
    pub music_dir: PathBuf,
    #[serde(default = "default_extensions")]
    pub extensions: Vec<String>,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub onboarding_done: bool,
    /// Name of the extractor used for online search (currently only "ytdlp").
    #[serde(default = "default_extractor")]
    pub extractor: String,
    /// Timeout for extractor search/resolve calls.
    #[serde(default = "default_timeout")]
    pub search_timeout_secs: u64,
    #[serde(default = "default_page_size")]
    pub page_size: usize,
    /// Loudness normalisation from ReplayGain tags: off, track or album.
    #[serde(default)]
    pub replaygain: ReplayGainMode,
    /// Winamp skin output: auto, kitty, iterm2, sixel or halfblock.
    #[serde(default = "default_graphics")]
    pub graphics: String,
    /// Media keys and the OS now-playing widget (MPRIS on Linux, Now
    /// Playing on macOS).
    #[serde(default = "default_true")]
    pub media_controls: bool,
    /// Radio, podcasts, lyrics, Subsonic and Jellyfin (`[providers.*]`).
    #[serde(default)]
    pub providers: ProvidersConfig,
}

fn default_music_dir() -> PathBuf {
    dirs::audio_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Music")))
        .unwrap_or_else(|| PathBuf::from("Music"))
}

fn default_extensions() -> Vec<String> {
    ["mp3", "flac", "ogg", "wav", "m4a", "aac", "opus", "wma"]
        .into_iter()
        .map(String::from)
        .collect()
}

fn default_theme() -> String {
    "Dark".into()
}

fn default_extractor() -> String {
    "ytdlp".into()
}

fn default_timeout() -> u64 {
    60
}

fn default_page_size() -> usize {
    30
}

fn default_graphics() -> String {
    "auto".into()
}

fn default_true() -> bool {
    true
}

/// `<config dir>/rustune` (e.g. `~/.config/rustune` on Linux).
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("rustune")
}

pub fn skins_dir() -> PathBuf {
    config_dir().join("skins")
}

pub fn playlists_dir() -> PathBuf {
    config_dir().join("playlists")
}

pub fn state_path() -> PathBuf {
    config_dir().join("state.toml")
}

/// `<cache dir>/rustune`, falling back to the config dir.
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .map(|d| d.join("rustune"))
        .unwrap_or_else(|| config_dir().join("cache"))
}

/// Expand a leading `~` / `~/` to the home directory.
pub fn expand_tilde(input: &str) -> PathBuf {
    if input == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    } else if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(input)
}

/// Write `contents` to `path` atomically (temp file + rename), creating parents.
pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

impl Config {
    /// Load the config; a parse/read error is returned alongside the defaults
    /// so the caller can surface it (stderr is unusable once the TUI runs).
    pub fn load() -> (Self, Option<String>) {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(content) => match toml::from_str(&content) {
                Ok(config) => (config, None),
                Err(e) => (
                    Self::default(),
                    Some(format!("Invalid {}: {e}", path.display())),
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (
                Self::default(),
                Some(format!("Cannot read {}: {e}", path.display())),
            ),
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let content = toml::to_string_pretty(self)?;
        write_atomic(&Self::path(), content.as_bytes())?;
        Ok(())
    }

    pub fn path() -> PathBuf {
        config_dir().join("config.toml")
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            music_dir: default_music_dir(),
            extensions: default_extensions(),
            theme: default_theme(),
            onboarding_done: false,
            extractor: default_extractor(),
            search_timeout_secs: default_timeout(),
            page_size: default_page_size(),
            replaygain: ReplayGainMode::Off,
            graphics: default_graphics(),
            media_controls: true,
            providers: ProvidersConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_config_with_mpv_args_still_parses() {
        let cfg: Config =
            toml::from_str("theme = \"Winamp\"\nmpv_args = [\"--foo\"]\nsearch_timeout_secs = 5\n")
                .expect("legacy config parses");
        assert_eq!(cfg.theme, "Winamp");
        assert_eq!(cfg.search_timeout_secs, 5);
        assert_eq!(cfg.page_size, 30);
        assert_eq!(cfg.replaygain, ReplayGainMode::Off);
        assert!(cfg.media_controls);
        assert!(cfg.providers.subsonic.is_none());
    }

    #[test]
    fn providers_section_parses_and_round_trips() {
        let text = "replaygain = \"album\"\n\
                    [providers.podcast]\nsubscriptions = [\"https://feed.example/rss\"]\n\
                    [providers.lyrics]\nenabled = false\n\
                    [providers.subsonic]\nurl = \"https://music.example\"\nuser = \"u\"\npassword = \"p\"\n";
        let cfg: Config = toml::from_str(text).expect("providers parse");
        assert_eq!(cfg.replaygain, ReplayGainMode::Album);
        assert_eq!(cfg.providers.podcast.subscriptions.len(), 1);
        assert!(!cfg.providers.lyrics.enabled);
        assert!(cfg.providers.subsonic.is_some());
        let again: Config =
            toml::from_str(&toml::to_string_pretty(&cfg).expect("serialize")).expect("reparse");
        assert_eq!(again.providers, cfg.providers);
    }

    #[test]
    fn tilde_expansion() {
        assert_eq!(expand_tilde("/abs/x"), PathBuf::from("/abs/x"));
        assert_eq!(expand_tilde("rel/x"), PathBuf::from("rel/x"));
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expand_tilde("~/Music"), home.join("Music"));
            assert_eq!(expand_tilde("~"), home);
        }
    }
}
