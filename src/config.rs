use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    #[serde(default = "default_extractor")]
    pub extractor: String,
    #[serde(default = "default_timeout")]
    pub search_timeout_secs: u64,
    #[serde(default = "default_page_size")]
    pub page_size: usize,
    #[serde(default)]
    pub mpv_args: Vec<String>,
}

fn default_music_dir() -> PathBuf {
    dirs::audio_dir().unwrap_or_else(|| PathBuf::from("~/Music")).join("")
}

fn default_extensions() -> Vec<String> {
    vec![
        "mp3".into(),
        "flac".into(),
        "ogg".into(),
        "wav".into(),
        "m4a".into(),
        "aac".into(),
        "opus".into(),
        "wma".into(),
    ]
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

impl Config {
    pub fn load() -> Self {
        let config_path = Self::config_path();

        if config_path.exists() {
            match std::fs::read_to_string(&config_path) {
                Ok(content) => match toml::from_str(&content) {
                    Ok(config) => return config,
                    Err(e) => eprintln!("Warning: failed to parse config: {e}"),
                },
                Err(e) => eprintln!("Warning: failed to read config: {e}"),
            }
        }

        Self::default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let config_path = Self::config_path();

        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = toml::to_string_pretty(self)?;
        std::fs::write(&config_path, content)?;
        Ok(())
    }

    fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("rustune")
            .join("config.toml")
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
            mpv_args: Vec::new(),
        }
    }
}
