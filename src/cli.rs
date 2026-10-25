//! Command-line parsing, `--help`, `doctor`, and turning positional
//! arguments (files, directories, URLs, playlists) into queue items.

use crate::config;
use crate::extractor;
use crate::install::{self, Dep, DepStatus};
use crate::library;
use crate::media::{MediaItem, SourceKind};
use crate::playlist_io;

#[derive(Debug, PartialEq, Eq)]
pub enum Cli {
    Help,
    Version,
    Doctor,
    /// Launch the TUI; non-empty `inputs` replace the queue and autoplay.
    Run {
        inputs: Vec<String>,
    },
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Cli, String> {
    let mut inputs = Vec::new();
    let mut only_inputs = false;
    let mut args = args.into_iter().peekable();
    if args.peek().map(String::as_str) == Some("doctor") {
        return match args.nth(1) {
            None => Ok(Cli::Doctor),
            Some(extra) => Err(format!("unexpected argument after 'doctor': {extra}")),
        };
    }
    for arg in args {
        if only_inputs {
            inputs.push(arg);
            continue;
        }
        match arg.as_str() {
            "-h" | "--help" => return Ok(Cli::Help),
            "-V" | "--version" => return Ok(Cli::Version),
            "--" => only_inputs = true,
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option: {s}")),
            _ => inputs.push(arg),
        }
    }
    Ok(Cli::Run { inputs })
}

pub fn help_text(program: &str) -> String {
    format!(
        "rustune {version} — terminal music player

USAGE:
    {program} [FILE|DIR|URL|PLAYLIST]...   Play the given items (or open the library)
    {program} doctor                       Check audio output, optional tools and paths
    {program} --help | --version

ARGUMENTS:
    FILE       Audio file (mp3, flac, ogg, wav, m4a, aac, alac, aiff, ...)
    DIR        Directory, scanned recursively for audio files
    URL        http(s) stream or radio playlist, or a YouTube URL (yt-dlp)
    PLAYLIST   .m3u / .m3u8 / .pls (entries relative to the playlist)

KEYS (transport, Winamp style; work everywhere in the player):
    z  previous   x  play   c  pause   v  stop   b  next   Space  play/pause
    ←/→  seek ∓5 s        Shift+←/→  seek ∓30 s
    + / = / -  volume     S  shuffle   R  repeat (off/all/one)
    e  EQ panel   P  playlist window   l  open file/URL   w  save queue as .m3u
    V  cycle visualizer    y  lyrics panel    q / Ctrl+C  quit

KEYS (library):
    /            search            j/k ↓/↑     move           g/G   first/last
    Enter        play list as queue           a     append to queue
    A            play next                    n/p   next/prev result page (online)
    Tab/S-Tab    source tab: Local, Online, Radio, Podcasts, Subsonic, Jellyfin
    d            download (online)            s     settings

KEYS (Radio, Podcasts, Subsonic, Jellyfin):
    Enter  open / play   Backspace/Esc  up a level   /  search   r  reload
    f      subscribe to / unsubscribe from a podcast

KEYS (playlist window, P):
    j/k  move   Enter  play   J/K  reorder   D/Del  remove   P  back to library

KEYS (EQ panel, e):
    ←/→ h/l  select slider   ↑/↓ k/j  ±1 dB   K/J  ±3 dB   0  reset slider
    o  EQ on/off             [ / ]  previous/next preset

COMMANDS (type after /):
    :q  quit   :clear  clear the queue   :eq <preset>|toggle|auto   :vol N   :bal N
    :seek 90|1:30|+10|-10   :repeat off|all|one   :shuffle   :preamp dB   :band 1-10 dB
    :play :pause :toggle :stop :next :prev :open

FILES:
    config      {config}
    session     {state}
    playlists   {playlists}
    skins       {skins}

Playback is built in (no mpv). Optional tools: yt-dlp (online search and
downloads), ffmpeg (opus/wma and HLS streams). Run '{program} doctor'.

MORE:
    https://rustune.dzulfikar.com",
        version = env!("CARGO_PKG_VERSION"),
        config = config::Config::path().display(),
        state = config::state_path().display(),
        playlists = config::playlists_dir().display(),
        skins = config::skins_dir().display(),
    )
}

/// Name of the default audio output device, if there is one.
fn output_device() -> Option<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let device = cpal::default_host().default_output_device()?;
    Some(
        device
            .description()
            .map_or_else(|_| "default".to_string(), |d| d.name().to_string()),
    )
}

/// `rustune doctor`: report the audio output, optional tools and paths.
/// Never fails; every external tool is optional.
pub async fn doctor() {
    println!("rustune {} — doctor\n", env!("CARGO_PKG_VERSION"));
    match output_device() {
        Some(name) => println!("  [ok] audio output: {name}"),
        None => println!("  [!!] audio output: no default output device found"),
    }
    let deps = install::probe_deps().await;
    let report = |dep: Dep, status: &DepStatus, purpose: &str| match status {
        DepStatus::Found(version) => println!("  [ok] {}: {version}", dep.binary()),
        _ => println!(
            "  [--] {}: not found (optional, {purpose}) — {}",
            dep.binary(),
            dep.homepage()
        ),
    };
    report(Dep::Ytdlp, &deps.ytdlp, "online search and downloads");
    report(Dep::Ffmpeg, &deps.ffmpeg, "opus/wma files and HLS streams");

    let dir = config::config_dir();
    if dir.exists() {
        println!("  [ok] config: {}", dir.display());
    } else {
        println!("  [--] config: {} (created on first run)", dir.display());
    }
    let (cfg, warning) = config::Config::load();
    if let Some(w) = warning {
        println!("  [!!] {w}");
    }
    if cfg.music_dir.is_dir() {
        println!("  [ok] music dir: {}", cfg.music_dir.display());
    } else {
        println!("  [--] music dir: {} (not found)", cfg.music_dir.display());
    }
    let mut sources = vec!["Local", "Online"];
    sources.extend(
        cfg.providers
            .enabled()
            .into_iter()
            .map(|k| k.display_name()),
    );
    println!("  [ok] sources: {}", sources.join(", "));

    println!("\nPlayback needs no external tools. Run 'rustune' to start.");
}

/// Expand positional arguments into queue items (blocking: walks
/// directories and reads tags). Unusable arguments are reported, not fatal.
pub fn resolve_inputs(inputs: &[String], extensions: &[String]) -> (Vec<MediaItem>, Vec<String>) {
    let mut items = Vec::new();
    let mut errors = Vec::new();
    for input in inputs {
        if let Some((name, id)) = extractor::parse_url(input) {
            items.push(MediaItem {
                title: input.clone(),
                id,
                artist: None,
                album: None,
                duration: None,
                subtitle: None,
                source: SourceKind::Extractor(name.to_string()),
            });
            continue;
        }
        if input.starts_with("http://") || input.starts_with("https://") {
            items.push(MediaItem {
                id: input.clone(),
                title: input.clone(),
                artist: None,
                album: None,
                duration: None,
                subtitle: None,
                source: SourceKind::Url,
            });
            continue;
        }
        let path = config::expand_tilde(input);
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        if path.is_dir() {
            match library::scan(&path, extensions, None) {
                Ok(found) if found.is_empty() => {
                    errors.push(format!("{input}: no audio files found"));
                }
                Ok(found) => items.extend(found),
                Err(e) => errors.push(format!("{input}: {e:#}")),
            }
        } else if playlist_io::is_playlist_path(&path) {
            match playlist_io::read_playlist(&path) {
                Ok(entries) => items.extend(entries),
                Err(e) => errors.push(format!("{input}: {e:#}")),
            }
        } else if path.is_file() {
            items.push(library::item_for_file(&path));
        } else {
            errors.push(format!("{input}: no such file or directory"));
        }
    }
    (items, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_flags_and_inputs() {
        assert_eq!(parse(args(&[])), Ok(Cli::Run { inputs: vec![] }));
        assert_eq!(parse(args(&["--help"])), Ok(Cli::Help));
        assert_eq!(parse(args(&["a.mp3", "-V"])), Ok(Cli::Version));
        assert_eq!(parse(args(&["doctor"])), Ok(Cli::Doctor));
        assert!(parse(args(&["doctor", "x"])).is_err());
        assert!(parse(args(&["--bogus"])).is_err());
        assert_eq!(
            parse(args(&["a.mp3", "--", "-weird.mp3", "--help"])),
            Ok(Cli::Run {
                inputs: args(&["a.mp3", "-weird.mp3", "--help"])
            })
        );
        assert_eq!(
            parse(args(&["-", "x/"])),
            Ok(Cli::Run {
                inputs: args(&["-", "x/"])
            })
        );
    }

    #[test]
    fn help_mentions_every_transport_key() {
        let help = help_text("rustune");
        for needle in [
            "z  previous",
            "Shift+←/→",
            "w  save queue",
            "V  cycle visualizer",
            "y  lyrics",
            "Backspace/Esc",
            "doctor",
            ".pls",
        ] {
            assert!(help.contains(needle), "{needle}");
        }
    }

    #[test]
    fn resolves_urls_files_dirs_and_reports_missing() {
        let dir = std::env::temp_dir().join(format!("rustune-cli-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("album")).unwrap();
        std::fs::write(dir.join("album/1.mp3"), b"x").unwrap();
        std::fs::write(dir.join("album/2.mp3"), b"x").unwrap();
        std::fs::write(dir.join("one.mp3"), b"x").unwrap();
        std::fs::write(dir.join("list.m3u"), "album/2.mp3\nhttp://r.example/s\n").unwrap();
        let exts = vec!["mp3".to_string()];
        let inputs = vec![
            "https://youtu.be/abcDEF12345".to_string(),
            "https://radio.example/live".to_string(),
            dir.join("album").to_string_lossy().into_owned(),
            dir.join("one.mp3").to_string_lossy().into_owned(),
            dir.join("list.m3u").to_string_lossy().into_owned(),
            dir.join("missing.mp3").to_string_lossy().into_owned(),
        ];
        let (items, errors) = resolve_inputs(&inputs, &exts);
        let kinds: Vec<&SourceKind> = items.iter().map(|i| &i.source).collect();
        assert_eq!(items.len(), 7, "{items:?}");
        assert_eq!(kinds[0], &SourceKind::Extractor("ytdlp".into()));
        assert_eq!(kinds[1], &SourceKind::Url);
        assert_eq!(items[2].title, "1");
        assert_eq!(items[3].title, "2");
        assert_eq!(items[4].title, "one");
        assert_eq!(items[5].title, "2");
        assert_eq!(kinds[6], &SourceKind::Url);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("missing.mp3"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
