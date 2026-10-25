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
- **Themes** — Dark, Light and Winamp; full mouse support

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
| `rustune FILE\|DIR\|URL\|PLAYLIST...` | Replace the queue with the given items and play. Directories are scanned recursively; URLs may be http(s) streams or YouTube links (via yt-dlp); playlists are `.m3u` / `.m3u8` / `.pls` |
| `rustune doctor` | Check optional tools and paths |
| `rustune --help` | Show help |
| `rustune --version` | Show version |

### Keybindings

#### Transport (Winamp style)

| Key | Action |
|-----|--------|
| `z` / `x` / `c` / `v` / `b` | Previous / play / pause / stop / next |
| `Space` | Play / pause |
| `←` / `→` | Seek −5 s / +5 s |
| `Shift+←` / `Shift+→` | Seek −30 s / +30 s |
| `+` / `=` / `-` | Volume up / up / down |
| `S` | Toggle shuffle |
| `R` | Cycle repeat (off / all / one) |
| `e` | EQ panel |
| `P` | Playlist window |
| `l` | Open file / URL |
| `V` | Cycle visualizer mode |
| `y` | Lyrics |
| `w` | Save queue as `.m3u` |
| `q` / `Ctrl+C` | Quit |

#### Library

| Key | Action |
|-----|--------|
| `/` | Search (or type a `:` command) |
| `j` / `k`, `↓` / `↑` | Move |
| `g` / `G`, `Home` / `End` | First / last item |
| `Enter` | Play the list as the queue, starting at the selection |
| `a` | Append to queue |
| `A` | Play next |
| `n` / `p` | Next / previous result page (online) |
| `d` | Download (online) |
| `Tab` / `Shift+Tab` | Next / previous source tab: Local, Online, Radio, Podcasts, Subsonic, Jellyfin |
| `s` | Settings |

#### Provider tabs (Radio, Podcasts, Subsonic, Jellyfin)

| Key | Action |
|-----|--------|
| `Enter` | Open folder / play item |
| `Backspace` / `Esc` | Up a level |
| `/` | Search |
| `f` | Subscribe / unsubscribe podcast |
| `r` | Reload the current level |

#### Playlist window (`P`)

| Key | Action |
|-----|--------|
| `j` / `k` | Move |
| `Enter` | Play |
| `J` / `K` | Move item down / up |
| `D` / `Del` | Remove |

#### EQ panel (`e`)

| Key | Action |
|-----|--------|
| `←` / `→`, `h` / `l` | Select band |
| `↑` / `↓`, `k` / `j` | Band ±1 dB |
| `K` / `J` | Band ±3 dB |
| `0` | Reset |
| `o` | EQ on / off |
| `[` / `]` | Previous / next preset |

#### Search prompt

| Key | Action |
|-----|--------|
| `Enter` | Submit |
| `Esc` | Cancel |
| `↑` / `↓` | Search history |
| `Ctrl+U` | Clear input |

#### Commands (type after `/`)

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

#### Mouse

Click a row to play it, use the wheel to scroll, and click the progress bar to seek. In Winamp mode every skin button and slider works with click and drag.

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

## Disclaimer

rustune is a personal music player. Online audio search is powered by [yt-dlp](https://github.com/yt-dlp/yt-dlp), a third-party tool that rustune does not control. Users are solely responsible for ensuring their use of any online sources complies with applicable laws and the terms of service of the respective platforms.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines on reporting bugs, suggesting features, and submitting pull requests.

## License

This project is licensed under the [MIT License](LICENSE).

## Sponsor

If you find rustune useful, consider supporting development:

[Donate via Saweria](https://saweria.co/dzulfikar08)
