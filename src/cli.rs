//! Command-line parsing, `--help`, `doctor`, and turning positional
//! arguments (files, directories, URLs, playlists) into queue items.

use crate::config;
use crate::extractor;
use crate::install::{self, Dep, DepStatus};
use crate::ipc::client::{self as remote, Remote};
use crate::library;
use crate::media::{MediaItem, SourceKind};
use crate::playlist_io;

#[derive(Debug, PartialEq)]
pub enum Cli {
    Help,
    Version,
    Doctor,
    /// Launch the TUI; non-empty `inputs` replace the queue and autoplay
    /// (or are queued in an instance that is already running).
    Run {
        inputs: Vec<String>,
    },
    /// `--daemon`: no TUI, controlled over IPC.
    Daemon {
        inputs: Vec<String>,
    },
    /// `play`, `status`, `volume 50`, ...: talk to the running instance.
    Remote(Remote),
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Cli, String> {
    let mut inputs = Vec::new();
    let mut only_inputs = false;
    let mut daemon = false;
    let mut args = args.into_iter().peekable();
    if args.peek().map(String::as_str) == Some("doctor") {
        return match args.nth(1) {
            None => Ok(Cli::Doctor),
            Some(extra) => Err(format!("unexpected argument after 'doctor': {extra}")),
        };
    }
    if let Some(name) = args
        .peek()
        .filter(|a| remote::SUBCOMMANDS.contains(&a.as_str()))
        .cloned()
    {
        let rest: Vec<String> = args.skip(1).collect();
        if rest.iter().any(|a| a == "-h" || a == "--help") {
            return Ok(Cli::Help);
        }
        return remote::parse(&name, &rest).map(Cli::Remote);
    }
    for arg in args {
        if only_inputs {
            inputs.push(arg);
            continue;
        }
        match arg.as_str() {
            "-h" | "--help" => return Ok(Cli::Help),
            "-V" | "--version" => return Ok(Cli::Version),
            "-d" | "--daemon" => daemon = true,
            "--" => only_inputs = true,
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option: {s}")),
            _ => inputs.push(arg),
        }
    }
    Ok(if daemon {
        Cli::Daemon { inputs }
    } else {
        Cli::Run { inputs }
    })
}

pub fn help_text(program: &str) -> String {
    let mut keys = String::new();
    crate::keymap::write_help(&mut keys);
    format!(
        "rustune {version}: terminal music player

USAGE:
    {program} [FILE|DIR|URL|PLAYLIST]...   Play the given items (or open the library);
                                           queued instead if rustune already runs
    {program} --daemon [ITEM]...           Run headless, controlled by the commands below
    {program} doctor                       Check audio output, optional tools and paths
    {program} --help | --version

REMOTE CONTROL (talks to the running player or daemon; exits 1 if none runs):
  Playback
    {program} play | pause | toggle | stop | next | prev | shuffle | quit
    {program} status [--json]              Now playing, time, volume, modes, queue, view
    {program} volume 0-100|+N|-N           Set or change the volume (percent)
    {program} seek SECS|MM:SS|+N|-N        Jump to a position or skip N seconds
    {program} repeat [off|all|one]         Set (or cycle) the repeat mode
    {program} eq PRESET|on|off|toggle      EQ preset (turns the EQ on) or on/off
    {program} watch                        Print a JSON status line on every change
  Queue (positions start at 1)
    {program} queue ITEM...                Append files, directories, playlists or URLs
    {program} list [--json]                The queue; > marks the playing track, * the cursor
    {program} jump N                       Play queue entry N
    {program} remove N                     Remove queue entry N
    {program} move FROM TO                 Move queue entry FROM to position TO
    {program} clear                        Stop and empty the queue
  Search (runs in the player, like its source tabs)
    {program} search SOURCE QUERY... [--limit N] [--json] [--queue | --play N]
        SOURCE: local, online (yt-dlp), radio, podcasts, subsonic, jellyfin.
        Prints numbered results. --queue appends every playable result;
        --play N replaces the queue with the results and plays result N.
        Podcast results are shows: --play N plays that show's newest episode.
        e.g. {program} search online lofi beats --play 1
  Look
    {program} theme dark|light|winamp      Switch the theme
    {program} vis MODE|next|list           Visualizer: bars, braille, led, mirror, scope,
                                           spectrogram, fire, none
    {program} skin NAME|MD5|FILE.wsz       Apply a Winamp skin (installed name, Skin
                                           Museum md5 to download, or a file)
    {program} lyrics [--json]              Lyrics of the current track (or why there are none)
  Driving the TUI (not in --daemon mode)
    {program} key KEY... [--text WORDS]    Press keys as if typed: j, ?, G, Enter, Esc, Tab,
                                           BackTab, Backspace, Up, Down, Left, Right, PgUp,
                                           PgDn, Home, End, Space, Del, F1-F12, ctrl+k,
                                           alt+x, shift+Right; --text types a string
    {program} screen [--ansi] [--json]     Print the last drawn frame (plain text, ANSI
                                           colours, or JSON with the prompt cursor)
  Anything else
    {program} send 'JSON'                  Send one protocol request, print the raw reply

Page URLs on youtube.com, youtu.be, music.youtube.com, soundcloud.com,
*.bandcamp.com, vimeo.com and mixcloud.com play through yt-dlp
(e.g. {program} https://youtu.be/ID); direct media URLs stream natively.

ARGUMENTS:
    FILE       Audio file (mp3, flac, ogg, wav, m4a, aac, alac, aiff, ...)
    DIR        Directory, scanned recursively for audio files
    URL        http(s) stream or radio playlist, or a YouTube, SoundCloud, Bandcamp,
               Vimeo or Mixcloud page (yt-dlp)
    PLAYLIST   .m3u / .m3u8 / .pls (entries relative to the playlist)

In the player, press ? or Ctrl+K for a searchable list of every command.

{keys}COMMANDS (type after / or :):
    :q  quit   :clear  clear the queue   :eq <preset>|toggle|auto   :vol N   :bal N
    :seek 90|1:30|+10|-10   :repeat off|all|one   :shuffle   :preamp dB   :band 1-10 dB
    :theme [dark|light|winamp]   :play :pause :toggle :stop :next :prev :open

FILES:
    config      {config}
    session     {state}
    playlists   {playlists}
    skins       {skins}
    socket      {socket}

The control socket speaks newline-delimited JSON: {{\"cmd\":\"next\"}},
{{\"cmd\":\"set_volume\",\"arg\":0.5}}, {{\"get\":\"status|queue|lyrics|screen\"}},
{{\"keys\":[\"j\"]}}, {{\"search\":{{...}}}}, {{\"skin\":\"...\"}}, {{\"subscribe\":true}}.
Set RUSTUNE_SOCKET to use another socket path.

Playback is built in (no mpv). Optional tools: yt-dlp (online search and
downloads), ffmpeg (opus/wma and HLS streams). Run '{program} doctor'.

MEDIA KEYS:
    Media keys and the system's now-playing controls drive the player (TUI and
    --daemon): MPRIS on Linux (playerctl, desktop widgets), Now Playing on
    macOS. Turn off with media_controls = false in the config. Not available
    on Windows.

MORE:
    https://rustune.dzulfikar.com",
        version = env!("CARGO_PKG_VERSION"),
        config = config::Config::path().display(),
        state = config::state_path().display(),
        playlists = config::playlists_dir().display(),
        skins = config::skins_dir().display(),
        socket = crate::ipc::endpoint().display(),
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
    println!("rustune {} doctor\n", env!("CARGO_PKG_VERSION"));
    match output_device() {
        Some(name) => println!("  [ok] audio output: {name}"),
        None => println!("  [!!] audio output: no default output device found"),
    }
    let deps = install::probe_deps().await;
    let report = |dep: Dep, status: &DepStatus, purpose: &str| match status {
        DepStatus::Found(version) => println!("  [ok] {}: {version}", dep.binary()),
        _ => println!(
            "  [--] {}: not found (optional, {purpose}), see {}",
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
    println!("  {}", crate::mediactl::doctor(cfg.media_controls));

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
    fn parses_daemon_and_remote_commands() {
        use crate::command::Command;
        assert_eq!(
            parse(args(&["--daemon", "a.wav"])),
            Ok(Cli::Daemon {
                inputs: args(&["a.wav"])
            })
        );
        assert_eq!(parse(args(&["-d"])), Ok(Cli::Daemon { inputs: vec![] }));
        assert_eq!(
            parse(args(&["next"])),
            Ok(Cli::Remote(Remote::Send(Command::Next)))
        );
        assert_eq!(
            parse(args(&["volume", "-5"])),
            Ok(Cli::Remote(Remote::Send(Command::VolumeBy(-0.05))))
        );
        assert_eq!(
            parse(args(&["status", "--json"])),
            Ok(Cli::Remote(Remote::Status { json: true }))
        );
        assert_eq!(parse(args(&["seek", "--help"])), Ok(Cli::Help));
        assert!(parse(args(&["volume"])).is_err());
        // Subcommand names only count first; later they are inputs.
        assert_eq!(
            parse(args(&["a.mp3", "play"])),
            Ok(Cli::Run {
                inputs: args(&["a.mp3", "play"])
            })
        );
    }

    #[test]
    fn help_lists_the_key_table() {
        let help = help_text("rustune");
        for b in crate::keymap::BINDINGS {
            assert!(help.contains(b.desc), "{}", b.desc);
        }
        for needle in [
            "Shift+←",
            "Backspace/Esc",
            "?/Ctrl+K",
            ":theme",
            "doctor",
            "--daemon",
            "status [--json]",
            "volume 0-100|+N|-N",
            "watch",
            ".pls",
            "list [--json]",
            "jump N",
            "remove N",
            "move FROM TO",
            "clear",
            "search SOURCE QUERY",
            "--play N",
            "newest episode",
            "theme dark|light|winamp",
            "vis MODE|next|list",
            "skin NAME|MD5|FILE.wsz",
            "lyrics [--json]",
            "key KEY... [--text WORDS]",
            "screen [--ansi] [--json]",
            "send 'JSON'",
            "soundcloud.com",
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
        assert_eq!(items[0].id, "https://youtu.be/abcDEF12345");
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

    #[test]
    fn inputs_are_classified_as_pages_media_urls_or_paths() {
        let dir = std::env::temp_dir().join(format!("rustune-cls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("song.mp3");
        std::fs::write(&file, b"x").unwrap();
        let inputs = vec![
            "https://soundcloud.com/artist/track".to_string(),
            "https://artist.bandcamp.com/album/lp".to_string(),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_string(),
            "https://cdn.example/stream.mp3".to_string(),
            "https://soundcloud.com/artist/clip.mp3".to_string(),
            file.to_string_lossy().into_owned(),
        ];
        let (items, errors) = resolve_inputs(&inputs, &["mp3".to_string()]);
        assert!(errors.is_empty(), "{errors:?}");
        let ytdlp = SourceKind::Extractor("ytdlp".into());
        let got: Vec<(&SourceKind, &str)> =
            items.iter().map(|i| (&i.source, i.id.as_str())).collect();
        assert_eq!(got[0], (&ytdlp, inputs[0].as_str()));
        assert_eq!(got[1], (&ytdlp, inputs[1].as_str()));
        assert_eq!(got[2], (&ytdlp, inputs[2].as_str()));
        assert_eq!(got[3], (&SourceKind::Url, inputs[3].as_str()));
        assert_eq!(got[4], (&SourceKind::Url, inputs[4].as_str()));
        assert_eq!(got[5].0, &SourceKind::Local);
        // Until yt-dlp names it, a page is titled by its URL.
        assert_eq!(items[0].title, inputs[0]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn new_remote_commands_route_to_the_remote() {
        assert_eq!(
            parse(args(&["list", "--json"])),
            Ok(Cli::Remote(Remote::List { json: true }))
        );
        assert_eq!(
            parse(args(&["key", "j", "j"])),
            Ok(Cli::Remote(Remote::Keys(args(&["j", "j"]))))
        );
        assert!(matches!(
            parse(args(&["search", "local", "tone"])),
            Ok(Cli::Remote(Remote::Search { .. }))
        ));
        assert_eq!(
            parse(args(&["https://youtu.be/x"])),
            Ok(Cli::Run {
                inputs: args(&["https://youtu.be/x"])
            })
        );
    }
}
