# rustune

A lightweight terminal music player: a single small binary with no mpv or other runtime dependencies. Native Rust audio engine, visualizer, synced lyrics, Internet radio, podcasts, self-hosted servers and pixel-accurate Winamp skins — all in your terminal.

**[Website](https://rustune.dzulfikar.com)** | **[Releases](https://github.com/dzulfikar08/rustune/releases)** | **[Contributing](CONTRIBUTING.md)**

> rustune is not affiliated with any content platform. Online audio search requires a third-party extractor (yt-dlp). Users are responsible for complying with the terms of service of any content they access.

## Features

- **Native audio engine** (cpal + symphonia) — gapless playback, 10-band EQ with Winamp presets and preamp, ReplayGain (track/album), balance, volume
- **Streams** — HTTP streams with ICY (now-playing) titles, radio playlists (`.pls` / `.m3u`); optional ffmpeg for exotic formats and HLS
- **Visualizer** — `bars`, `braille`, `led`, `mirror`, `scope`, `spectrogram`, `fire`, or `none`
- **Lyrics** — fetched from [LRCLIB](https://lrclib.net), synced line-by-line when available
- **Sources**
  - Local library (MP3, FLAC, OGG, WAV, M4A, AAC, OPUS, WMA)
  - Online search and download via yt-dlp (optional)
  - Internet radio ([Radio Browser](https://www.radio-browser.info))
  - Podcasts (iTunes charts and search, subscriptions)
  - Subsonic / Navidrome and Jellyfin / Emby, when configured
- **Winamp 2.x skins** (`.wsz`) — pixel-accurate rendering with kitty, iTerm2 or sixel graphics, or a half-block fallback; built-in browser for the [Winamp Skin Museum](https://skins.webamp.org/)
- **Playlists** — load and save `.m3u` / `.pls`
- **Session restore** — volume, balance, EQ, shuffle/repeat, visualizer, queue and resume position
- **Themes** — Dark, Light and Winamp (`t` cycles, `:theme light` picks); full mouse support
- **Command palette** — `?` / `Ctrl+K` lists every action for the current view with its key and current value, fuzzy-filtered as you type
- **Remote control** — `rustune next`, `rustune status --json`, `rustune watch`, … over a local socket; a headless `--daemon` mode; `rustune FILE` queues into the running player instead of starting a second one
- **Media keys and now-playing widgets** — MPRIS2 on Linux (`playerctl`, GNOME/KDE media controls), Now Playing and the media keys on macOS; in the TUI and in `--daemon` mode

## Screenshots

> TODO: Add screenshots here

## Installation

### One-liner (macOS / Linux)

```bash
curl -sL https://github.com/dzulfikar08/rustune/releases/latest/download/rustune-$(uname -m)-macos -o /usr/local/bin/rustune && chmod +x /usr/local/bin/rustune
```

> On Linux, replace `macos` with `linux` in the URL.

### From Release

Download the latest binary for your platform from the [Releases](https://github.com/dzulfikar08/rustune/releases) page.

**macOS users**: Download the `.dmg` for a signed & notarized app experience, or the bare binary if you prefer the terminal-only approach.

#### Installing the bare binary

```bash
mkdir -p ~/.local/bin
cp ~/Downloads/rustune-aarch64-macos ~/.local/bin/rustune
chmod +x ~/.local/bin/rustune
```

> On Linux, the file is `rustune-x86_64-linux`. On Intel Macs, use `rustune-x86_64-macos`.

Then add `~/.local/bin` to your PATH (if not already) in `~/.zshrc`:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

After that, just run `rustune` from anywhere.

### Requirements

- To build: [Rust](https://rustup.rs/) (latest stable). On Linux, the ALSA development headers (`libasound2-dev` on Debian/Ubuntu, `alsa-lib-devel` on Fedora).
- Optional at runtime:
  - [yt-dlp](https://github.com/yt-dlp/yt-dlp) — online search and download
  - [ffmpeg](https://ffmpeg.org/) — formats symphonia can't decode, and HLS streams

### From Source (cargo)

```bash
cargo install --git https://github.com/dzulfikar08/rustune
```

### From Source (manual)

```bash
git clone https://github.com/dzulfikar08/rustune.git
cd rustune
cargo build --release
cp target/release/rustune /usr/local/bin/
```

## First Run

When you launch rustune for the first time, it will guide you through a short setup:

1. **Music directory** — Point rustune to your local music folder
2. **Theme** — Choose between Dark, Light, or Winamp style
3. **Ready** — Start browsing and playing

To check optional tools and paths, run:

```bash
rustune doctor
```

## Usage

```bash
rustune [FILE|DIR|URL|PLAYLIST]...
```

| Command | Description |
|---------|-------------|
| `rustune` | Launch the TUI (restores the last session) |
| `rustune FILE\|DIR\|URL\|PLAYLIST...` | Replace the queue with the given items and play. Directories are scanned recursively; URLs may be http(s) streams or YouTube links (via yt-dlp); playlists are `.m3u` / `.m3u8` / `.pls`. If rustune is already running, the items are appended to its queue instead |
| `rustune --daemon [ITEM...]` | Run without a terminal UI (same engine, queue and session state), controlled with the commands below; SIGINT/SIGTERM save the session and exit |
| `rustune doctor` | Check optional tools and paths |
| `rustune --help` | Show help |
| `rustune --version` | Show version |

### Keybindings

Press `?` or `Ctrl+K` anywhere in the player for the **command palette**: every action that works in the current view, with its keys, a description and the current value of toggles (shuffle, repeat, EQ, visualizer, theme, …). Type to fuzzy-filter, `↑`/`↓` to pick, `Enter` to run. The hint bar at the bottom shows the most useful keys for the current view.

The tables below are generated from the key table in `src/keymap.rs` (the same table the key handler and the palette read); `cargo test` fails when they are out of date.

<!-- keymap:start -->
#### Playback

| Key | Action |
|-----|--------|
| `z` | Previous track |
| `x` | Play (restarts a playing track) |
| `Space` / `c` | Pause / resume |
| `v` | Stop |
| `b` | Next track |
| `←` | Seek back 5 s |
| `→` | Seek forward 5 s |
| `Shift+←` | Seek back 30 s |
| `Shift+→` | Seek forward 30 s |
| `Ctrl+J` | Jump to a time (:seek) |
| `+` / `=` | Volume up |
| `-` | Volume down |
| `S` | Toggle shuffle |
| `R` | Cycle repeat (off, all, one) |
| `o` | Turn the EQ on or off |
| `[` | Previous EQ preset |
| `]` | Next EQ preset |
| *palette* | Stop and clear the queue |

#### Panels, themes and general

| Key | Action |
|-----|--------|
| `e` | Toggle the EQ panel |
| `P` | Toggle the playlist window |
| `y` | Toggle the lyrics panel |
| `V` | Cycle the visualizer mode |
| `t` | Cycle the theme (Dark, Light, Winamp) |
| `l` | Open a file, folder, URL or playlist |
| `w` | Save the queue as an .m3u playlist |
| `Tab` | Next source tab |
| `Shift+Tab` | Previous source tab |
| `s` | Settings |
| `:` | Type a : command (e.g. :theme light) |
| `?` / `Ctrl+K` | Command palette: every action, filterable |
| `q` / `Ctrl+C` | Quit |

#### Lists (library, provider tabs, playlist window)

| Key | Action |
|-----|--------|
| `j` / `↓` | Move down |
| `k` / `↑` | Move up |
| `g` / `Home` | First item |
| `G` / `End` | Last item |
| `PgDn` | Page down |
| `PgUp` | Page up |
| `Enter` | Play the selection (library: the list from there; providers: open folders) |
| `a` | Append the selection to the queue |
| `A` | Play the selection next |

#### Library (Local and Online tabs)

| Key | Action |
|-----|--------|
| `/` | Search (or type a : command) |
| `n` | Next result page (Online) |
| `p` | Previous result page (Online) |
| `d` | Download the selection (Online) |

#### Provider tabs (Radio, Podcasts, Subsonic, Jellyfin)

| Key | Action |
|-----|--------|
| `/` | Search this source |
| `Backspace` / `Esc` | Up a level |
| `r` | Reload the current level |
| `f` | Subscribe to or unsubscribe from the podcast |

#### Playlist window (P)

| Key | Action |
|-----|--------|
| `J` | Move the track down |
| `K` | Move the track up |
| `D` / `Del` | Remove the track from the queue |

#### EQ panel (e)

| Key | Action |
|-----|--------|
| `←` / `h` | Previous slider |
| `→` / `l` | Next slider |
| `↑` / `k` | Raise the slider 1 dB |
| `↓` / `j` | Lower the slider 1 dB |
| `K` | Raise the slider 3 dB |
| `J` | Lower the slider 3 dB |
| `0` | Reset the slider to 0 dB |
| `Esc` | Close the EQ panel |

#### Prompt (search, : commands, save, open)

| Key | Action |
|-----|--------|
| `Enter` | Submit |
| `Esc` | Cancel |
| `↑` | Previous search |
| `↓` | Next search |
| `←` | Cursor left |
| `→` | Cursor right |
| `Home` / `Ctrl+A` | Start of the line |
| `End` / `Ctrl+E` | End of the line |
| `Backspace` | Delete before the cursor |
| `Del` | Delete at the cursor |
| `Ctrl+U` | Clear the input |

#### Command palette (? or Ctrl+K)

| Key | Action |
|-----|--------|
| `↑` / `Ctrl+P` | Previous entry |
| `↓` / `Ctrl+N` | Next entry |
| `Enter` | Run the selected entry |
| `Backspace` | Delete a filter character |
| `Ctrl+U` | Clear the filter |
| `Esc` / `Ctrl+K` | Close the palette |

<!-- keymap:end -->

#### Commands (type after `/` or `:`)

| Command | Action |
|---------|--------|
| `:q` | Quit |
| `:clear` | Clear the queue |
| `:eq <preset>` / `:eq toggle` / `:eq auto` | Load an EQ preset / toggle EQ / auto preset |
| `:vol N` | Set volume |
| `:bal N` | Set balance |
| `:seek 90` / `:seek 1:30` / `:seek +10` / `:seek -10` | Seek absolute or relative |
| `:repeat off\|all\|one` | Set repeat mode |
| `:shuffle` | Toggle shuffle |
| `:play` / `:pause` / `:toggle` / `:stop` | Transport |
| `:next` / `:prev` | Next / previous track |
| `:preamp dB` | Set EQ preamp |
| `:band 1-10 dB` | Set one EQ band |
| `:open` | Open file / URL |
| `:theme` / `:theme dark\|light\|winamp` | Cycle the theme / switch to a theme |

#### Mouse

Click a row to play it, use the wheel to scroll, and click the progress bar to seek. In Winamp mode every skin button and slider works with click and drag.

### Remote control

A running player (TUI or `--daemon`) listens on a local control socket: `$XDG_RUNTIME_DIR/rustune.sock`, falling back to the config directory (mode `0600`), or the named pipe `\\.\pipe\rustune` on Windows. `RUSTUNE_SOCKET` overrides the path, e.g. to run two instances. A socket left behind by a crash is detected and replaced.

| Command | Action |
|---------|--------|
| `rustune play` / `pause` / `toggle` / `stop` / `next` / `prev` | Transport |
| `rustune status [--json]` | Now playing, time, volume, balance, shuffle, repeat, EQ, queue position |
| `rustune volume 0-100\|+N\|-N` | Set or change the volume (percent) |
| `rustune seek SECS\|MM:SS\|+N\|-N` | Jump to a position or skip N seconds |
| `rustune shuffle` | Toggle shuffle |
| `rustune repeat [off\|all\|one]` | Set the repeat mode (cycle without an argument) |
| `rustune eq PRESET\|on\|off\|toggle` | Apply a preset (turns the EQ on), or switch the EQ |
| `rustune queue ITEM...` | Append files, directories, playlists or URLs |
| `rustune watch` | Print a JSON status line on every change |
| `rustune quit` | Stop the running instance |

The commands exit with status 1 and a message when no instance is running. The protocol is newline-delimited JSON, one object per line, so scripts can talk to the socket directly (e.g. `socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/rustune.sock`):

```text
{"cmd":"toggle_pause"}                      → {"ok":true}
{"cmd":"set_volume","arg":0.5}              → {"ok":true}
{"cmd":"enqueue","arg":["/music/a.flac"]}   → {"ok":true}
{"get":"status"}                            → {"status":"playing","title":"…","artist":"…","album":"…","elapsed":42,"duration":215,"volume":50,"balance":0,"shuffle":false,"repeat":"off","eq_enabled":true,"queue_length":12,"queue_index":3}
{"subscribe":true}                          → the status object now and after every change
{"cmd":"dance"}                             → {"error":"bad command: …"}
```

`cmd` names are the player commands in snake case (`play`, `pause`, `toggle_pause`, `stop`, `next`, `prev`, `seek_by`, `seek_frac`, `set_volume`, `volume_by`, `set_balance`, `toggle_shuffle`, `cycle_repeat`, `set_repeat`, `toggle_eq`, `eq_preset`, `set_eq_band`, `set_preamp`, `play_index`, `enqueue`, `quit`, …) with any argument in `arg`. A command is answered once the player has applied it.

## Configuration

Config file: `~/.config/rustune/config.toml` on Linux, `~/Library/Application Support/rustune/config.toml` on macOS.

```toml
music_dir = "~/Music"
extensions = ["mp3", "flac", "ogg", "wav", "m4a", "aac", "opus", "wma"]
theme = "Dark"
extractor = "ytdlp"
search_timeout_secs = 60
page_size = 30
replaygain = "off"
graphics = "auto"
media_controls = true
```

| Option | Default | Description |
|--------|---------|-------------|
| `music_dir` | `~/Music` | Directory to scan for local music |
| `extensions` | `mp3, flac, ogg, ...` | Audio file formats to recognize |
| `theme` | `Dark` | `Dark`, `Light`, or `Winamp` |
| `extractor` | `ytdlp` | Extractor used for online search |
| `search_timeout_secs` | `60` | Timeout for online searches |
| `page_size` | `30` | Results per page |
| `replaygain` | `off` | `off`, `track`, or `album` |
| `graphics` | `auto` | Skin graphics: `auto`, `kitty`, `iterm2`, `sixel`, or `halfblock` |
| `media_controls` | `true` | Media keys and the OS now-playing widget (see below) |

### Media keys

With `media_controls = true` (the default) rustune shows up in the system's media controls, and media keys control it — in the TUI and in `--daemon` mode alike:

- **Linux**: an MPRIS2 player on the D-Bus session bus (`org.mpris.MediaPlayer2.rustune`). Play, pause, stop, next, previous, seek, volume, shuffle and loop status all work, so do `playerctl` and desktop widgets; local files get cover art from a `cover.jpg`/`folder.jpg` next to them. rustune speaks D-Bus itself (no libdbus); without a session bus, media controls simply stay off.
- **macOS**: Now Playing (title, artist, album, duration, position) and the media keys / Control Center (play, pause, next, previous, scrubbing).
- **Windows**: not supported yet.

`rustune doctor` reports whether media controls are available.

### Providers

```toml
[providers.radio]
enabled = true
server = ""        # Radio Browser API host; empty = discover a mirror
country = ""       # ISO 3166-1 alpha-2 code for "stations in my country"

[providers.podcast]
enabled = true
country = "us"     # iTunes store front for the charts
subscriptions = [] # feed URLs (managed with `f` in the Podcasts tab)

[providers.lyrics]
enabled = true
server = "https://lrclib.net"

# Optional: configure to enable the Subsonic tab.
[providers.subsonic]
url = "https://music.example.com"
user = "me"
password = "secret"
# format = "opus"     # transcoding target (mp3, opus, raw, ...)
# max_bitrate = 192   # kbit/s
scrobble = true

# Optional: configure to enable the Jellyfin tab.
[providers.jellyfin]
url = "https://jellyfin.example.com"
user = "me"
password = "secret"
# token = "..."       # API key instead of user/password
# user_id = "..."     # skips user discovery when a token is set
# max_bitrate = 320   # kbit/s (default 320)
```

Jellyfin stores the session token in the config after the first login.

### Session state

`state.toml` (next to `config.toml`) is written on exit and restored on start: volume, balance, EQ, shuffle/repeat, visualizer mode (`vis`), and the queue with the resume position.

## Winamp Skins

Choose the **Winamp** theme in Settings (`s`). From there, press `i` to pick a local skin (`.wsz` files in the `skins/` folder of the config directory) or `o` to browse and download skins from the [Winamp Skin Museum](https://skins.webamp.org/).

The terminal graphics protocol (kitty, iTerm2, sixel, or half-block fallback) is detected automatically; override it with the `graphics` config option.

In Winamp mode the main, equalizer and playlist windows come from the skin's own bitmaps, and every button, slider and playlist row works with the mouse (click, drag, double-click to play, wheel to scroll). The playlist window shows the play queue: `l` opens files, folders, playlists or URLs, `/` loads matching library tracks (or yt-dlp results on the Online tab) into it, and `e` / `P` toggle the equalizer and playlist windows. Clicking the visualizer cycles analyzer → oscilloscope → off. Graphics need at least 275 columns in half-block mode; smaller terminals get a skin-coloured text layout. Provider browsing (radio, podcasts, Subsonic, Jellyfin) and the lyrics panel live in the Dark and Light themes.

## Disclaimer

rustune is a personal music player. Online audio search is powered by [yt-dlp](https://github.com/yt-dlp/yt-dlp), a third-party tool that rustune does not control. Users are solely responsible for ensuring their use of any online sources complies with applicable laws and the terms of service of the respective platforms.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines on reporting bugs, suggesting features, and submitting pull requests.

## License

This project is licensed under the [MIT License](LICENSE).

## Sponsor

If you find rustune useful, consider supporting development:

[Donate via Saweria](https://saweria.co/dzulfikar08)
