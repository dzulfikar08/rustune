# yewtube-rs: Rust TUI YouTube Player — Design Spec

**Date**: 2026-04-02
**Status**: Approved
**Scope**: Minimal player (search + play via mpv)

## Overview

A Rust TUI rewrite of yewtube (Python terminal YouTube player). Minimal scope: search YouTube, play audio via mpv. No download, no playlists, no integrations.

## Project Setup

- **Location**: `/Users/macbookpro/Documents/CobaCoba/yewtube-rs/`
- **Architecture**: Single crate, flat module structure
- **Edition**: Rust 2021

## Dependencies

```toml
[package]
name = "yewtube-rs"
version = "0.1.0"
edition = "2021"

[dependencies]
ratatui = "0.29"
crossterm = "0.28"
tokio = { version = "1", features = ["rt-multi-thread", "process", "sync", "time", "macros", "io-util"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
```

## Module Structure

```
src/
├── main.rs          # Terminal init, event loop, teardown, panic hook
├── app.rs           # App state struct + state transitions
├── ui/
│   ├── mod.rs       # Top-level draw function, layout
│   ├── results.rs   # Search results list rendering
│   ├── player.rs    # Now-playing bar rendering
│   └── input.rs     # Input bar rendering
├── youtube.rs       # yt-dlp subprocess: search + stream URL extraction
├── player.rs        # mpv subprocess management + IPC socket communication
└── event.rs         # App event types (search done, playback updates)
```

## UI Layout

Three zones stacked vertically:

```
┌──────────────────────────────────────────┐
│ Search Results (scrollable list)    ~70% │
│  1. Artist - Song Title      3:42       │
│ >>2. Artist - Song Title     4:15       │
│  3. Artist - Song Title      2:58       │
├──────────────────────────────────────────┤
│ ♪ Now Playing: Artist - Song   1:23/4:15│  <- player bar (1 line)
├──────────────────────────────────────────┤
│ /search term_                            │  <- input bar (1 line)
└──────────────────────────────────────────┘
```

### Widgets

- **Results area**: ratatui `List` widget with `ListState`. Each item shows index, title, duration. Highlighted selection via `highlight_style` (bold, yellow fg on dark gray bg).
- **Player bar**: ratatui `Paragraph` showing current track title, elapsed/total time, play/pause state.
- **Input bar**: ratatui `Paragraph` with cursor rendering. The input text and cursor position tracked in `App`.

### Color Scheme

| Element | Style |
|---------|-------|
| Result title (normal) | `Color::White` |
| Result duration | `Color::DarkGray` |
| Result title (selected) | `Color::Yellow`, bold, `Color::DarkGray` bg |
| Player bar text | `Color::Cyan` |
| Player bar elapsed | `Color::Green` |
| Input bar prompt (`/`) | `Color::Yellow` |
| Input bar text | `Color::White` |
| Status/error messages | `Color::Red` |
| Loading indicator | `Color::Yellow` |

## Interaction Model

### Modes

- **Browse mode** (default): Navigate results, trigger playback
- **Input mode**: Type search queries or commands

### Key Bindings

| Key | Mode | Action |
|-----|------|--------|
| `/` | Browse | Focus input, clear input text (switch to input mode) |
| `Enter` | Browse | Play selected result |
| `Enter` | Input | If input starts with `:`, parse as command. Otherwise, execute search and return to browse |
| `Esc` | Input | Cancel input, return to browse |
| `j` / `↓` | Browse | Move selection down |
| `k` / `↑` | Browse | Move selection up |
| `g` / `Home` | Browse | Jump to top of results |
| `G` / `End` | Browse | Jump to bottom of results |
| `n` | Browse | Next page of results (new yt-dlp search with offset) |
| `p` | Browse | Previous page of results |
| `Space` | Browse | Toggle pause via mpv IPC |
| `q` / `Ctrl+C` | Browse | Quit (kills mpv, restores terminal) |
| `:q` | Input | Quit command |

### Input Mode Editing

Standard input editing keys in input mode:

| Key | Action |
|-----|--------|
| `Backspace` | Delete character before cursor |
| `Delete` | Delete character at cursor |
| `Left` / `Right` | Move cursor left/right |
| `Home` / `Ctrl+A` | Move cursor to start |
| `End` / `Ctrl+E` | Move cursor to end |
| `Ctrl+U` | Clear entire input |
| `Up` / `Down` | Cycle through input history |

### Command System (Input Mode)

Lines starting with `:` are commands, not search queries:
- `:q` — quit the application
- `:help` — show key bindings (future, out of scope for v0.1)

All other input is treated as a YouTube search query.

## Data Flow

### Search Flow

```
User types query + Enter
  -> Set app.status = Status::Searching("Searching...")
  -> Redraw shows "Searching..." in results area
  -> Spawn tokio task:
       Run: yt-dlp "ytsearch10:query" --dump-json --no-download
       Parse each JSON line into SearchResult
       Send AppEvent::SearchComplete(results) through mpsc channel
  -> Main loop receives event, clears status, updates results, redraws
```

### Playback Flow

```
User selects track (Enter or number key)
  -> If already playing: kill current mpv process, clear PlaybackState
  -> Set app.status = Status::Loading("Loading stream...")
  -> Spawn tokio task:
       Run: yt-dlp -f "bestaudio/best" --get-url "https://youtube.com/watch?v=ID"
       Send AppEvent::StreamUrlReady(url or error)
  -> Main loop receives StreamUrlReady:
       On error: show "Playback failed" in player bar
       On success: spawn mpv task (see below)

Mpv task:
  -> Generate temp IPC socket path: /tmp/yewtube-mpv-{pid}.sock
  -> Spawn: mpv --no-video --idle=no --input-ipc-server={socket_path} <url>
  -> Background reader task: poll socket for time-pos every 500ms
  -> Send AppEvent::PlaybackProgress { elapsed_secs, duration_secs } periodically
  -> On mpv exit: send AppEvent::PlaybackComplete
```

### Pagination

Search returns up to 10 results per page. `n`/`p` keys trigger a new yt-dlp search with an offset. The app tracks `page: usize` and computes `ytsearch{page*10+1}-{(page+1)*10}` for next pages.

## Key Types

```rust
enum Mode {
    Browse,
    Input,
}

enum Status {
    Idle,
    Searching(String),
    Loading(String),
    Error(String),
}

struct App {
    mode: Mode,
    results: Vec<SearchResult>,
    list_state: ListState,
    page: usize,
    input_text: String,
    input_cursor: usize,          // byte offset within input_text
    input_history: Vec<String>,   // in-memory search history
    history_index: usize,         // current position in history
    playback: Option<PlaybackState>,
    status: Status,
    should_quit: bool,
}

struct SearchResult {
    id: String,           // YouTube video ID
    title: String,
    duration_secs: Option<u64>,  // None for live streams
    channel: Option<String>,     // may be empty from yt-dlp search
}

struct PlaybackState {
    title: String,
    duration_secs: u64,
    elapsed_secs: u64,
    paused: bool,
}
```

## Event System

Async events communicated via `tokio::sync::mpsc`:

```rust
enum AppEvent {
    SearchComplete(anyhow::Result<Vec<SearchResult>>),
    StreamUrlReady(anyhow::Result<String>),
    PlaybackProgress { elapsed_secs: u64, duration_secs: u64 },
    PlaybackComplete,
    PlaybackError(String),
}
```

### Event Loop (tokio::select!)

The main loop uses `tokio::select!` to wait on either crossterm events or mpsc events simultaneously — no busy-waiting:

```rust
// Bridge crossterm events into a tokio channel
let (tx_term, rx_term) = tokio::sync::mpsc::channel(100);
tokio::spawn(async move {
    loop {
        if crossterm::event::poll(Duration::from_millis(100)).unwrap() {
            tx_term.send(crossterm::event::read().unwrap()).await.unwrap();
        }
    }
});

loop {
    tokio::select! {
        Some(term_event) = rx_term.recv() => { /* handle key/mouse/resize */ }
        Some(app_event) = rx_app.recv() => { /* handle search/playback events */ }
    }
    terminal.draw(|frame| ui::draw(frame, &app))?;
    if app.should_quit { break; }
}
```

## mpv IPC Protocol

mpv is launched with `--input-ipc-server=/tmp/yewtube-mpv-{pid}.sock`. A background tokio task communicates over this Unix socket:

### Pause/Resume
```json
{ "command": ["set_property", "pause", true] }
{ "command": ["set_property", "pause", false] }
```

### Progress Tracking (poll every 500ms)
```json
{ "command": ["get_property", "time-pos"] }
{ "command": ["get_property", "duration"] }
```

The background task parses responses and sends `AppEvent::PlaybackProgress` through the mpsc channel.

### Process Ownership

The mpv child process is owned by a dedicated tokio task, not stored in `App`. The task holds `tokio::process::Child` (which is `Send`), waits on exit, and sends `PlaybackComplete`. `App` only stores the `PlaybackState` (metadata), not the process handle.

## External Dependencies (System)

- **yt-dlp**: Must be installed. Used for search (`ytsearchN:` prefix + `--dump-json --no-download`) and stream URL extraction (`-f "bestaudio/best" --get-url`).
- **mpv**: Must be installed. Used for audio playback with `--no-video --input-ipc-server`.

### Startup Validation

On launch, probe for `yt-dlp` and `mpv` via `which` (or `Command::new("yt-dlp").arg("--version")`). If either is missing, display an error screen with install instructions and exit after keypress.

## Error Handling

- **yt-dlp not found**: Detected at startup. Error screen with install instructions.
- **mpv not found**: Detected at startup. Error screen with install instructions.
- **No results**: Show "No results found." in list area.
- **Network timeout**: 30s timeout on yt-dlp commands, show "Search timed out."
- **Stream extraction failure**: Show "Playback failed: <reason>" in player bar.
- **Terminal resize**: Layout adapts automatically via `frame.area()` on each draw.
- **Panic recovery**: `main.rs` installs a panic hook that calls `ratatui::restore()` before printing the panic message, so the terminal is never left in raw mode.

### Stop-Before-Play

When a new track is selected while another is playing, the old mpv process is killed before the new one starts. This prevents overlapping audio.

### Graceful Quit

On quit (`q` / `Ctrl+C` / `:q`): kill any running mpv subprocess, remove IPC socket file, then restore terminal and exit.

## Explicitly Out of Scope

These features are intentionally excluded from v0.1:
- Download functionality
- Playlists (local or YouTube)
- Last.fm scrobbling
- MPRIS D-Bus integration
- Spotify import
- Album search
- Comments browser
- Config file / settings persistence
- Multi-digit result selection (e.g., `12` to select track 12)
- Automatic sequential playback (playing track 2 after track 1 finishes)
