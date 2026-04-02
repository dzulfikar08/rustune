# rustune

A terminal music player with online audio search, local playback, and Winamp skin support. Built in Rust with a beautiful TUI.

**[Website](https://rustune.dzulfikar.com)** | **[Releases](https://github.com/dzulfikar08/rustune/releases)** | **[Contributing](CONTRIBUTING.md)**

> rustune is not affiliated with any content platform. Online audio search requires a third-party extractor (yt-dlp). Users are responsible for complying with the terms of service of any content they access.

## Features

- **Local music playback** — Scan and play music from your filesystem (MP3, FLAC, OGG, WAV, M4A, AAC, OPUS, WMA)
- **Online audio search** — Search and stream audio from online sources via yt-dlp
- **Winamp skins** — Load classic Winamp 2.x `.wsz` skins, or browse and download from the online gallery
- **Multiple themes** — Dark, Light, and Winamp themes built in
- **Mouse support** — Click to play, pause, seek, and navigate
- **Pagination** — Browse large collections page by page
- **Search history** — Navigate through previous searches
- **First-run setup** — Guided onboarding for new users

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

### From Source (cargo)

```bash
cargo install --git https://github.com/dzulfikar08/rustune
```

### From Source (manual)

Requirements:
- [Rust](https://rustup.rs/) (latest stable)
- [mpv](https://mpv.io/) — media player backend
- [yt-dlp](https://github.com/yt-dlp/yt-dlp) — for online audio search and streaming

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

To check that all dependencies are installed, run:

```bash
rustune doctor
```

## Usage

Run `rustune` in your terminal:

```bash
rustune
```

### CLI Commands

| Command | Description |
|---------|-------------|
| `rustune` | Launch the TUI |
| `rustune doctor` | Check dependencies and system status |
| `rustune --help` | Show help |
| `rustune --version` | Show version |

### Keybindings

#### Browse Mode

| Key | Action |
|-----|--------|
| `/` | Enter search |
| `j` / `Down` | Move down |
| `k` / `Up` | Move up |
| `g` / `Home` | First item |
| `G` / `End` | Last item |
| `n` | Next page |
| `p` | Previous page |
| `Space` | Toggle pause |
| `Enter` | Play selected |
| `s` | Open settings |
| `Tab` | Switch source (Local / Online) |
| `q` | Quit |

#### Search Mode

| Key | Action |
|-----|--------|
| `Enter` | Submit search |
| `Esc` | Cancel search |
| `Up` / `Down` | Search history |
| `Ctrl+U` | Clear input |

## Configuration

Config file: `~/.config/rustune/config.toml`

```toml
music_dir = "~/Music"
extensions = ["mp3", "flac", "ogg", "wav", "m4a", "aac", "opus", "wma"]
theme = "Dark"
page_size = 30
search_timeout_secs = 60
mpv_args = []
```

### Options

| Option | Default | Description |
|--------|---------|-------------|
| `music_dir` | `~/Music` | Directory to scan for local music |
| `extensions` | `mp3, flac, ogg, ...` | Audio file formats to recognize |
| `theme` | `Dark` | Theme name: `Dark`, `Light`, or `Winamp` |
| `page_size` | `30` | Results per page |
| `search_timeout_secs` | `60` | Timeout for online searches |
| `mpv_args` | `[]` | Extra arguments passed to mpv |

## Winamp Skins

### Local Skins

Place `.wsz` files in `~/.config/rustune/skins/` and select one from Settings (`s`).

### Online Skin Browser

From Settings, choose "Skins" to browse and download skins from the [Winamp Skin Museum](https://skins.webamp.org/) directly within the app.

## Disclaimer

rustune is a personal music player. Online audio search is powered by [yt-dlp](https://github.com/yt-dlp/yt-dlp), a third-party tool that rustune does not control. Users are solely responsible for ensuring their use of any online sources complies with applicable laws and the terms of service of the respective platforms.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines on reporting bugs, suggesting features, and submitting pull requests.

## License

This project is licensed under the [MIT License](LICENSE).

## Sponsor

If you find rustune useful, consider supporting development:

[Donate via Saweria](https://saweria.co/dzulfikar08)
