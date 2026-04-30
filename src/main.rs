mod app;
mod config;
mod event;
mod extractor;
mod media;
mod player;
mod skin;
mod source;
mod theme;
mod ui;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

use app::{App, BrowseAction, InputAction, Mode, MouseAction, OnboardingAction, SettingsAction, SkinBrowserAction, Status};
use event::AppEvent;
use extractor::{ExtractorRegistry, ExtractorStatus, YtdlpExtractor};
use media::SourceKind;
use ui::skin_layout::SkinLayout;

fn main() -> Result<()> {
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        eprintln!("{info}");
        default_panic(info);
    }));

    let args: Vec<String> = std::env::args().collect();
    let program = args.get(0).map(|s| s.as_str()).unwrap_or("rustune");

    match args.get(1).map(|s| s.as_str()) {
        Some("--help") | Some("-h") => {
            print_help(program);
            Ok(())
        }
        Some("--version") | Some("-V") => {
            println!("rustune {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("doctor") => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(run_doctor());
            Ok(())
        }
        Some(other) => {
            eprintln!("Unknown argument: {other}");
            eprintln!("Run '{program} --help' for usage.");
            std::process::exit(1);
        }
        None => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(async_main())
        }
    }
}

fn print_help(program: &str) {
    println!(
        "rustune {} — terminal music player

USAGE:
    {program}              Launch the TUI
    {program} doctor       Check dependencies and system status
    {program} --help       Show this help message
    {program} --version    Show version

KEYBINDINGS (inside TUI):
    /          Search
    j/k        Move up/down
    Space      Toggle pause
    Enter      Play selected
    Tab        Switch source (Local / Online)
    s          Settings
    q          Quit

CONFIG:
    ~/.config/rustune/config.toml

MORE:
    https://rustune.dzulfikar.com",
        env!("CARGO_PKG_VERSION"),
    );
}

async fn run_doctor() {
    println!("rustune {} — doctor\n", env!("CARGO_PKG_VERSION"));

    // Check mpv
    let mpv_ok = match player::check_mpv().await {
        Ok(()) => {
            // Try to get version
            let output = tokio::process::Command::new("mpv")
                .arg("--version")
                .output()
                .await
                .ok();
            let version = output
                .and_then(|o| {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    let first_line = stdout.lines().next().unwrap_or("");
                    Some(first_line.to_string())
                })
                .unwrap_or_else(|| "unknown".to_string());
            println!("  [ok] mpv: {}", version.split_whitespace().take(2).collect::<Vec<_>>().join(" "));
            true
        }
        Err(e) => {
            println!("  [MISSING] mpv: {e}");
            println!("            Install from: https://mpv.io");
            false
        }
    };

    // Check yt-dlp
    let ytdlp_ok = match std::process::Command::new("yt-dlp")
        .arg("--version")
        .output()
    {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("  [ok] yt-dlp: {version}");
            true
        }
        Ok(_) => {
            println!("  [BROKEN] yt-dlp: installed but not working");
            false
        }
        Err(_) => {
            println!("  [MISSING] yt-dlp: not found");
            println!("            Install from: https://github.com/yt-dlp/yt-dlp");
            false
        }
    };

    // Check config directory
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("~/.config"))
        .join("rustune");
    if config_dir.exists() {
        println!("  [ok] config: {}", config_dir.display());
    } else {
        println!("  [--] config: {} (will be created on first run)", config_dir.display());
    }

    // Check music directory
    let music_dir = dirs::audio_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Music")))
        .unwrap_or_else(|| std::path::PathBuf::from("~/Music"));
    if music_dir.exists() {
        println!("  [ok] music dir: {}", music_dir.display());
    } else {
        println!("  [--] music dir: {} (not found)", music_dir.display());
    }

    println!();

    if mpv_ok && ytdlp_ok {
        println!("All good! Run 'rustune' to start playing.");
    } else if mpv_ok {
        println!("mpv is ready. Install yt-dlp for online audio search.");
    } else {
        println!("mpv is required for playback. Install it first.");
        std::process::exit(1);
    }
}

async fn async_main() -> Result<()> {
    let config = config::Config::load();

    // Ensure the default Winamp skin (base-2.91) is available on disk
    ensure_default_skin().await;

    // Check mpv (required for all playback)
    if let Err(e) = player::check_mpv().await {
        if config.onboarding_done {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }

    let terminal = ratatui::init();
    crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = run(terminal, config).await;
    crossterm::execute!(std::io::stdout(), DisableMouseCapture)?;
    ratatui::restore();
    result
}

async fn run(mut terminal: DefaultTerminal, config: config::Config) -> Result<()> {
    let mut app = App::new(config);

    // Load Winamp skin if theme is Winamp
    if app.theme.name == "Winamp" {
        app.winamp_skin = load_winamp_skin();
        app.skin_layout = app.winamp_skin.as_ref().and_then(SkinLayout::from_skin);
    }

    // Set up extractor registry
    let mut registry = ExtractorRegistry::new();
    let ytdlp = Arc::new(YtdlpExtractor::new());
    registry.add(ytdlp);
    let registry = Arc::new(registry);

    // If onboarding is done and active source supports browse, auto-scan local
    if app.config.onboarding_done {
        let _ = scan_local(&mut app);
    }

    let (tx_app, mut rx_app) = mpsc::unbounded_channel::<AppEvent>();

    let (tx_term, mut rx_term) = mpsc::channel(100);
    tokio::spawn(async move {
        loop {
            if crossterm::event::poll(Duration::from_millis(100)).is_ok() {
                if let Ok(event) = crossterm::event::read() {
                    if tx_term.send(event).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    loop {
        terminal.draw(|frame| ui::draw(frame, &mut app))?;

        // Check for pending dependency install (must happen outside terminal.draw)
        if let Some(dep_idx) = app.pending_install.take() {
            let (mpv_cmd, ytdlp_cmd) = get_install_commands();
            let (label, cmd) = if dep_idx == 0 {
                ("mpv", mpv_cmd)
            } else {
                ("yt-dlp", ytdlp_cmd)
            };

            if let Some((program, args)) = cmd {
                let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                let (success, new_terminal) = install_dependency(label, &program, &args_ref);
                terminal = new_terminal;
                if success {
                    app.status = Status::Idle;
                } else {
                    app.status = Status::Error(format!("{label} installation failed"));
                }
            } else {
                // No auto-install available (e.g. Windows)
                ratatui::restore();
                if dep_idx == 0 {
                    println!("\n  Please install mpv manually from https://mpv.io");
                } else {
                    println!("\n  Please install yt-dlp manually from https://github.com/yt-dlp/yt-dlp");
                }
                println!("\nPress Enter to return to rustune...");
                let mut buf = String::new();
                let _ = std::io::stdin().read_line(&mut buf);
                terminal = ratatui::init();
                let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
            }
            continue;
        }

        tokio::select! {
            Some(term_event) = rx_term.recv() => {
                match term_event {
                    Event::Key(key) => {
                        if key.kind != KeyEventKind::Press {
                            continue;
                        }
                        handle_key(&mut app, key, &tx_app, &registry);
                    }
                    Event::Mouse(mouse) => {
                        handle_mouse(&mut app, mouse, &tx_app, &registry);
                    }
                    _ => {}
                }
            }
            Some(app_event) = rx_app.recv() => {
                handle_app_event(&mut app, app_event, &tx_app, &registry);
            }
        }

        if app.should_quit {
            break;
        }
    }

    // Save config on exit
    let _ = app.config.save();

    Ok(())
}

fn handle_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    tx: &mpsc::UnboundedSender<AppEvent>,
    registry: &Arc<ExtractorRegistry>,
) {
    match app.mode {
        Mode::Browse => {
            let action = app.handle_browse_key(key);
            match action {
                BrowseAction::Play(id, title) => {
                    play_item(app, id, title, tx, registry);
                }
                BrowseAction::NextPage => {
                    if !app.results.is_empty() {
                        app.page += 1;
                        dispatch_search(app, tx, registry);
                    }
                }
                BrowseAction::PrevPage => {
                    if app.page > 0 {
                        app.page -= 1;
                        dispatch_search(app, tx, registry);
                    }
                }
                BrowseAction::TogglePause => {
                    if let Some(ref playback) = app.playback {
                        let new_paused = !playback.paused;
                        if let Some(ref mut pb) = app.playback {
                            pb.paused = new_paused;
                        }
                        tokio::spawn(async move {
                            let _ = player::set_pause(new_paused).await;
                        });
                    }
                }
                BrowseAction::OpenSettings => {
                    app.mode = Mode::Settings;
                }
                BrowseAction::SwitchSource => {
                    app.active_source = match app.active_source {
                        SourceKind::Local => {
                            if registry.first_available().is_some() {
                                SourceKind::Extractor("ytdlp".into())
                            } else {
                                SourceKind::Local
                            }
                        }
                        SourceKind::Extractor(_) => SourceKind::Local,
                    };
                    app.results.clear();
                    app.list_state.select(Some(0));
                    app.page = 0;
                    if app.active_source == SourceKind::Local {
                        let _ = scan_local(app);
                    }
                }
                BrowseAction::Download(id, title) => {
                    download_audio(app, id, title, tx);
                }
                BrowseAction::None => {}
            }
        }
        Mode::Input => {
            let action = app.handle_input_key(key);
            match action {
                InputAction::Search(_query) => {
                    if matches!(app.active_source, SourceKind::Local) {
                        search_local(app);
                    } else {
                        dispatch_search(app, tx, registry);
                    }
                }
                InputAction::None => {}
            }
        }
        Mode::Settings => {
            let action = app.handle_settings_key(key);
            match action {
                SettingsAction::Quit => {
                    let _ = app.config.save();
                }
                SettingsAction::ThemeChanged => {
                    app.winamp_skin = load_winamp_skin();
                    app.skin_layout = app.winamp_skin.as_ref().and_then(SkinLayout::from_skin);
                }
                SettingsAction::OpenSkinBrowserLocal => {
                    app.mode = Mode::SkinBrowser;
                    app.skin_browser_source = app::SkinBrowserSource::Local;
                    app.skin_entries.clear();
                    app.skin_list_state.select(Some(0));
                    app.skin_browser_loading = false;
                    app.skin_browser_error = None;
                    app.skin_browser_offset = 0;
                    app.skin_browser_has_more = false;
                    app.skin_downloading_md5 = None;
                    app.skin_total_count = 0;
                    app.skin_search_query.clear();
                    app.skin_search_active = false;
                    // Populate with local skins
                    let local_skins = skin::WinampSkin::available_skins();
                    for p in &local_skins {
                        let filename = p.file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let display_name = skin::WinampSkin::peek_metadata(p)
                            .ok()
                            .map(|(name, _author, _desc)| name)
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or_else(|| {
                                p.file_stem()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("unknown")
                                    .to_string()
                            });
                        app.skin_entries.push(app::SkinEntry {
                            md5: String::new(),
                            filename,
                            display_name,
                            is_local: true,
                            nsfw: false,
                            average_color: None,
                        });
                    }
                    if !app.skin_entries.is_empty() {
                        app.skin_list_state.select(Some(0));
                    }
                }
                SettingsAction::OpenSkinBrowserOnline => {
                    app.mode = Mode::SkinBrowser;
                    app.skin_browser_source = app::SkinBrowserSource::Online;
                    app.skin_entries.clear();
                    app.skin_list_state.select(Some(0));
                    app.skin_browser_loading = true;
                    app.skin_browser_error = None;
                    app.skin_browser_offset = 0;
                    app.skin_browser_has_more = true;
                    app.skin_downloading_md5 = None;
                    app.skin_total_count = 0;
                    app.skin_search_query.clear();
                    app.skin_search_active = false;
                    fetch_skin_list(tx, 0, 50);
                }
                SettingsAction::None => {}
            }
        }
        Mode::SkinBrowser => {
            let action = app.handle_skin_browser_key(key);
            match action {
                SkinBrowserAction::Back => {}
                SkinBrowserAction::Download(md5, filename) => {
                    download_skin(tx, &md5, &filename);
                }
                SkinBrowserAction::LoadLocal(filename) => {
                    let skin_dir = dirs::config_dir()
                        .unwrap_or_else(|| std::path::PathBuf::from("~/.config"))
                        .join("rustune")
                        .join("skins");
                    let skin_path = skin_dir.join(&filename);
                    match skin::WinampSkin::from_wsz(&skin_path) {
                        Ok(skin) => {
                            app.winamp_skin = Some(skin);
                            app.skin_layout = app.winamp_skin.as_ref().and_then(SkinLayout::from_skin);
                        }
                        Err(e) => {
                            app.skin_browser_error = Some(format!("Failed to load skin: {e}"));
                        }
                    }
                }
                SkinBrowserAction::RequestFetch => {
                    app.skin_browser_loading = true;
                    fetch_skin_list(tx, app.skin_browser_offset, 50);
                }
                SkinBrowserAction::Search(query) => {
                    app.skin_entries.clear();
                    app.skin_list_state.select(Some(0));
                    app.skin_browser_loading = true;
                    app.skin_browser_error = None;
                    app.skin_browser_offset = 0;
                    app.skin_browser_has_more = false;
                    fetch_skin_search(tx, &query, 0, 50);
                }
                SkinBrowserAction::None => {}
            }
        }
        Mode::Onboarding => {
            let action = app.handle_onboarding_key(key);
            match action {
                OnboardingAction::SetMusicDir(dir) => {
                    let path = std::path::PathBuf::from(&dir);
                    if path.exists() {
                        app.config.music_dir = path;
                    }
                }
                OnboardingAction::SelectTheme(name) => {
                    app.config.theme = name;
                    let _ = app.config.save();
                    let _ = scan_local(app);
                }
                OnboardingAction::Next | OnboardingAction::None => {}
            }
        }
    }
}

fn dispatch_search(
    app: &mut App,
    tx: &mpsc::UnboundedSender<AppEvent>,
    registry: &Arc<ExtractorRegistry>,
) {
    let query = match app.mode {
        Mode::Input => app.input_history.last().cloned().unwrap_or_default(),
        _ => app.input_history.last().cloned().unwrap_or_default(),
    };

    if query.is_empty() {
        return;
    }

    app.status = Status::Searching("Searching...".into());
    app.results.clear();
    app.list_state.select(Some(0));

    let page = app.page;
    let limit = app.config.page_size;
    let offset = page * limit;
    let source = app.active_source.clone();

    if let Some(extractor) = registry.first_available() {
        let ext = extractor.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let result = ext.search(&query, offset, limit).await;
            let _ = tx.send(AppEvent::ItemsReady {
                results: result,
                source,
            });
        });
    } else {
        app.status = Status::Error("No extractor available for search".into());
    }
}

fn play_item(
    app: &mut App,
    id: String,
    title: String,
    tx: &mpsc::UnboundedSender<AppEvent>,
    registry: &Arc<ExtractorRegistry>,
) {
    app.kill_mpv();
    app.status = Status::Loading("Loading stream...".into());

    match app.active_source {
        SourceKind::Local => {
            // Local file — construct file:// URL directly
            let url = format!("file://{}", id);
            start_playback(app, url, title, tx);
            let _ = title; // used in start_playback closure
        }
        SourceKind::Extractor(ref name) => {
            if let Some(extractor) = registry.get(name) {
                let ext = extractor.clone();
                let tx = tx.clone();
                let id = id.clone();
                let title = title.clone();
                tokio::spawn(async move {
                    let result = ext.resolve(&id, &title).await;
                    let _ = tx.send(AppEvent::StreamReady(result));
                });
            } else {
                app.status = Status::Error(format!("Extractor '{}' not found", name));
            }
        }
    }
}

fn start_playback(
    app: &mut App,
    url: String,
    title: String,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    app.status = Status::Idle;
    app.playback = Some(app::PlaybackState {
        title: title.clone(),
        duration_secs: 0,
        elapsed_secs: 0,
        paused: false,
    });

    let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();
    app.mpv_kill = Some(kill_tx);

    let tx = tx.clone();
    tokio::spawn(async move {
        player::play(url, title, tx, kill_rx).await;
    });
}

fn scan_local(app: &mut App) -> Result<()> {
    let music_dir = app.config.music_dir.clone();
    let extensions = app.config.extensions.clone();

    app.status = Status::Scanning("Scanning music directory...".into());

    let source = source::LocalSource::new(music_dir, extensions);
    let items = source.scan_sync();

    match items {
        Ok(items) => {
            app.local_library = items.clone();
            app.results = items;
            app.status = Status::Idle;
            if !app.results.is_empty() {
                app.list_state.select(Some(0));
            }
            Ok(())
        }
        Err(e) => {
            app.status = Status::Error(format!("Scan failed: {e}"));
            Err(e)
        }
    }
}

fn search_local(app: &mut App) {
    let query = app.input_history.last().cloned().unwrap_or_default();
    if query.is_empty() {
        app.results = app.local_library.clone();
    } else {
        app.results = source::LocalSource::search(&app.local_library, &query);
    }
    app.list_state.select(Some(0));
    app.page = 0;
    app.status = Status::Idle;
}

fn handle_app_event(
    app: &mut App,
    event: AppEvent,
    tx: &mpsc::UnboundedSender<AppEvent>,
    _registry: &Arc<ExtractorRegistry>,
) {
    match event {
        AppEvent::ItemsReady { results, source: _ } => {
            match results {
                Ok(items) => {
                    app.results = items;
                    app.status = Status::Idle;
                    if !app.results.is_empty() {
                        app.list_state.select(Some(0));
                    }
                }
                Err(e) => {
                    app.status = Status::Error(format!("Search failed: {e}"));
                }
            }
        }
        AppEvent::StreamReady(result) => {
            match result {
                Ok(info) => {
                    start_playback(app, info.url, info.title, tx);
                }
                Err(e) => {
                    app.status = Status::Error(format!("Playback failed: {e}"));
                }
            }
        }
        AppEvent::PlaybackProgress {
            elapsed_secs,
            duration_secs,
        } => {
            if let Some(ref mut pb) = app.playback {
                pb.elapsed_secs = elapsed_secs;
                pb.duration_secs = duration_secs;
            }
        }
        AppEvent::PlaybackComplete => {
            app.playback = None;
            app.mpv_kill = None;
        }
        AppEvent::PlaybackError(msg) => {
            app.playback = None;
            app.mpv_kill = None;
            app.status = Status::Error(msg);
        }
        AppEvent::ExtractorStatus { name, status } => {
            match status {
                ExtractorStatus::Available(_) => {
                    app.status = Status::Idle;
                }
                ExtractorStatus::NotFound | ExtractorStatus::Broken(_) => {
                    if app.active_source == SourceKind::Extractor(name.clone()) {
                        app.active_source = SourceKind::Local;
                    }
                }
            }
        }
        AppEvent::ScanComplete(result) => {
            match result {
                Ok(items) => {
                    app.results = items;
                    app.status = Status::Idle;
                    if !app.results.is_empty() {
                        app.list_state.select(Some(0));
                    }
                }
                Err(e) => {
                    app.status = Status::Error(format!("Scan failed: {e}"));
                }
            }
        }
        AppEvent::SkinListFetched { entries, requested_offset } => {
            app.skin_browser_loading = false;
            match entries {
                Ok((new_entries, total_count)) => {
                    if requested_offset == 0 {
                        app.skin_entries = new_entries;
                    } else {
                        app.skin_entries.extend(new_entries);
                        // Re-sort entire list alphabetically after append
                        app.skin_entries.sort_by(|a, b| {
                            a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase())
                        });
                    }
                    app.skin_total_count = total_count;
                    app.skin_browser_has_more = app.skin_entries.len() < total_count;
                    if app.skin_list_state.selected().is_none() && !app.skin_entries.is_empty() {
                        app.skin_list_state.select(Some(0));
                    }
                }
                Err(e) => {
                    app.skin_browser_error = Some(format!("{e}"));
                }
            }
        }
        AppEvent::SkinDownloaded { md5: _, filename, result } => {
            app.skin_downloading_md5 = None;
            match result {
                Ok(()) => {
                    let skin_dir = dirs::config_dir()
                        .unwrap_or_else(|| std::path::PathBuf::from("~/.config"))
                        .join("rustune")
                        .join("skins");
                    let safe_filename = if filename.ends_with(".wsz") {
                        filename.clone()
                    } else {
                        format!("{filename}.wsz")
                    };
                    let skin_path = skin_dir.join(&safe_filename);
                    match skin::WinampSkin::from_wsz(&skin_path) {
                        Ok(skin) => {
                            app.winamp_skin = Some(skin);
                            app.skin_layout = app.winamp_skin.as_ref().and_then(SkinLayout::from_skin);
                        }
                        Err(e) => {
                            app.skin_browser_error = Some(format!("Failed to load skin: {e}"));
                        }
                    }
                }
                Err(e) => {
                    app.skin_browser_error = Some(format!("Download failed: {e}"));
                }
            }
        }
        AppEvent::DownloadComplete { title, result } => {
            app.downloading_title = None;
            match result {
                Ok(()) => {
                    app.active_source = SourceKind::Local;
                    let _ = scan_local(app);
                    app.status = Status::Idle;
                }
                Err(e) => {
                    app.status = Status::Error(format!("Download failed ({title}): {e}"));
                }
            }
        }
    }
}

fn handle_mouse(
    app: &mut App,
    mouse: crossterm::event::MouseEvent,
    tx: &mpsc::UnboundedSender<AppEvent>,
    registry: &Arc<ExtractorRegistry>,
) {
    let action = app.handle_mouse(mouse);
    match action {
        MouseAction::PlaySelected => {
            if let Some(result) = app.selected_result() {
                let id = result.id.clone();
                let title = result.title.clone();
                play_item(app, id, title, tx, registry);
            }
        }
        MouseAction::Seek(position_secs) => {
            if app.playback.is_some() {
                tokio::spawn(async move {
                    let _ = player::seek_to(position_secs).await;
                });
            }
        }
        MouseAction::TogglePause => {
            if let Some(ref playback) = app.playback {
                let new_paused = !playback.paused;
                if let Some(ref mut pb) = app.playback {
                    pb.paused = new_paused;
                }
                tokio::spawn(async move {
                    let _ = player::set_pause(new_paused).await;
                });
            }
        }
        MouseAction::PrevPage => {
            if app.page > 0 {
                app.page -= 1;
                dispatch_search(app, tx, registry);
            }
        }
        MouseAction::NextPage => {
            if !app.results.is_empty() {
                app.page += 1;
                dispatch_search(app, tx, registry);
            }
        }
        MouseAction::ScrollUp | MouseAction::ScrollDown | MouseAction::SelectResult(_) | MouseAction::None => {}
    }
}

/// Suspend the TUI, run an install command in the terminal, then resume.
/// Returns (success, new_terminal).
fn install_dependency(label: &str, program: &str, args: &[&str]) -> (bool, DefaultTerminal) {
    // Restore terminal so user can see the install output
    ratatui::restore();

    let args_str = args.join(" ");
    println!("\nInstalling {label}...");
    println!("  $ {program} {args_str}");

    let success = match std::process::Command::new(program)
        .args(args)
        .status()
    {
        Ok(status) if status.success() => {
            println!("\n  {label} installed successfully.");
            true
        }
        Ok(status) => {
            println!("\n  Installation failed with exit code: {}", status.code().unwrap_or(-1));
            false
        }
        Err(e) => {
            println!("\n  Failed to run install command: {e}");
            false
        }
    };

    println!("\nPress Enter to return to rustune...");
    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);

    // Re-initialize terminal
    let terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);

    (success, terminal)
}

#[cfg(unix)]
fn get_install_commands() -> (Option<(String, Vec<String>)>, Option<(String, Vec<String>)>) {
    // mpv install command
    let mpv_cmd = if std::path::Path::new("/opt/homebrew/bin/brew").exists()
        || which_exists("brew")
    {
        Some(("brew".to_string(), vec!["install".to_string(), "mpv".to_string()]))
    } else if which_exists("apt") {
        Some(("sudo".to_string(), vec!["apt".to_string(), "install".to_string(), "-y".to_string(), "mpv".to_string()]))
    } else {
        None
    };

    // yt-dlp install command
    let ytdlp_cmd = if std::path::Path::new("/opt/homebrew/bin/brew").exists()
        || which_exists("brew")
    {
        Some(("brew".to_string(), vec!["install".to_string(), "yt-dlp".to_string()]))
    } else if which_exists("pipx") {
        Some(("pipx".to_string(), vec!["install".to_string(), "yt-dlp".to_string()]))
    } else if which_exists("pip") {
        Some(("pip".to_string(), vec!["install".to_string(), "yt-dlp".to_string()]))
    } else {
        None
    };

    (mpv_cmd, ytdlp_cmd)
}

#[cfg(not(unix))]
fn get_install_commands() -> (Option<(String, Vec<String>)>, Option<(String, Vec<String>)>) {
    // No auto-install on Windows
    (None, None)
}

#[cfg(unix)]
fn which_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Download the default Winamp skin (base-2.91) if no skins exist locally.
async fn ensure_default_skin() {
    let skin_dir = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("~/.config"))
        .join("rustune")
        .join("skins");

    let default_path = skin_dir.join("base-2.91.wsz");
    if default_path.exists() {
        return;
    }

    let md5 = "5e4f10275dcb1fb211d4a8b4f1bda236";
    let url = format!("https://r2.webampskins.org/skins/{md5}.wsz");

    let client = reqwest::Client::new();
    if let Ok(resp) = client.get(&url).send().await {
        if let Ok(bytes) = resp.bytes().await {
            let _ = std::fs::create_dir_all(&skin_dir);
            let _ = std::fs::write(&default_path, &bytes);
        }
    }
}

/// Try to load a .wsz skin file. Checks:
/// 1. Any .wsz files in ~/.config/rustune/skins/
/// 2. Falls back to built-in default Winamp skin colors
fn load_winamp_skin() -> Option<skin::WinampSkin> {
    let skins = skin::WinampSkin::available_skins();
    if let Some(first_skin) = skins.first() {
        match skin::WinampSkin::from_wsz(first_skin) {
            Ok(skin) => {
                eprintln!("Loaded Winamp skin: {}", skin.name);
                return Some(skin);
            }
            Err(e) => {
                eprintln!("Warning: failed to load skin {}: {e}", first_skin.display());
            }
        }
    }
    // No skin files found — use built-in defaults
    Some(skin::WinampSkin::default_skin())
}

/// Fetch skin listings from skins.webamp.org GraphQL API.
fn fetch_skin_list(tx: &mpsc::UnboundedSender<AppEvent>, offset: usize, limit: usize) {
    let tx = tx.clone();
    tokio::spawn(async move {
        let result = fetch_skin_list_inner(offset, limit).await;
        let _ = tx.send(AppEvent::SkinListFetched {
            entries: result,
            requested_offset: offset,
        });
    });
}

/// Search skins by text query via GraphQL.
fn fetch_skin_search(
    tx: &mpsc::UnboundedSender<AppEvent>,
    query: &str,
    offset: usize,
    limit: usize,
) {
    let tx = tx.clone();
    let query = query.to_string();
    tokio::spawn(async move {
        let result = fetch_skin_search_inner(&query, offset, limit).await;
        let _ = tx.send(AppEvent::SkinListFetched {
            entries: result,
            requested_offset: offset,
        });
    });
}

async fn fetch_skin_search_inner(
    query: &str,
    offset: usize,
    limit: usize,
) -> anyhow::Result<(Vec<app::SkinEntry>, usize)> {
    let gql = serde_json::json!({
        "query": "query($q: String!, $first: Int!, $offset: Int!) { search_skins(first: $first, offset: $offset, query: $q) { md5 filename nsfw average_color } }",
        "variables": { "q": query, "first": limit, "offset": offset }
    });

    let client = reqwest::Client::new();
    let resp = client
        .post("https://skins.webamp.org/graphql")
        .json(&gql)
        .send()
        .await?
        .error_for_status()?;

    let body: serde_json::Value = resp.json().await?;

    let skins_arr = body["data"]["search_skins"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    let local_skins = skin::WinampSkin::available_skins();
    let local_md5s: Vec<String> = local_skins
        .iter()
        .filter_map(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_lowercase())
        })
        .collect();

    let mut entries: Vec<app::SkinEntry> = skins_arr
        .into_iter()
        .filter(|s| !s["nsfw"].as_bool().unwrap_or(false))
        .map(|s| {
            let md5 = s["md5"].as_str().unwrap_or("").to_string();
            let raw_filename = s["filename"].as_str().unwrap_or("").to_string();
            let display_name = raw_filename.trim_end_matches(".wsz").trim_end_matches(".zip").to_string();
            let is_local = local_md5s.iter().any(|l| l == &md5.to_lowercase());
            let average_color = s["average_color"].as_str().map(|c| c.to_string());
            app::SkinEntry {
                md5,
                filename: raw_filename,
                display_name,
                is_local,
                nsfw: false,
                average_color,
            }
        })
        .collect();

    let total = entries.len();
    entries.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));

    Ok((entries, total))
}

async fn fetch_skin_list_inner(
    offset: usize,
    limit: usize,
) -> anyhow::Result<(Vec<app::SkinEntry>, usize)> {
    let query = serde_json::json!({
        "query": "query($first: Int!, $offset: Int!) { skins(first: $first, offset: $offset, filter: APPROVED) { nodes { md5 filename nsfw average_color } count } }",
        "variables": { "first": limit, "offset": offset }
    });

    let client = reqwest::Client::new();
    let resp = client
        .post("https://skins.webamp.org/graphql")
        .json(&query)
        .send()
        .await?
        .error_for_status()?;

    let body: serde_json::Value = resp.json().await?;

    let total_count = body["data"]["skins"]["count"]
        .as_u64()
        .unwrap_or(0) as usize;

    let skins_arr = body["data"]["skins"]["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    let local_skins = skin::WinampSkin::available_skins();
    let local_md5s: Vec<String> = local_skins
        .iter()
        .filter_map(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_lowercase())
        })
        .collect();

    let mut entries: Vec<app::SkinEntry> = skins_arr
        .into_iter()
        .filter(|s| !s["nsfw"].as_bool().unwrap_or(false))
        .map(|s| {
            let md5 = s["md5"].as_str().unwrap_or("").to_string();
            let raw_filename = s["filename"].as_str().unwrap_or("").to_string();
            let display_name = raw_filename.trim_end_matches(".wsz").trim_end_matches(".zip").to_string();
            let is_local = local_md5s.iter().any(|l| l == &md5.to_lowercase());
            let average_color = s["average_color"].as_str().map(|c| c.to_string());
            app::SkinEntry {
                md5,
                filename: raw_filename,
                display_name,
                is_local,
                nsfw: false,
                average_color,
            }
        })
        .collect();

    // Sort alphabetically by display name
    entries.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));

    Ok((entries, total_count))
}

fn download_audio(
    app: &mut App,
    id: String,
    title: String,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    let music_dir = app.config.music_dir.clone();
    app.downloading_title = Some(title.clone());
    app.status = Status::Downloading(title.clone());

    let tx = tx.clone();
    tokio::spawn(async move {
        let result = download_audio_inner(&id, &title, &music_dir).await;
        let _ = tx.send(AppEvent::DownloadComplete {
            title,
            result,
        });
    });
}

async fn download_audio_inner(id: &str, title: &str, music_dir: &std::path::Path) -> anyhow::Result<()> {
    let safe_title: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect();
    let output_template = music_dir.join(format!("{safe_title}.%(ext)s"));
    let url = format!("https://www.youtube.com/watch?v={id}");

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        tokio::process::Command::new("yt-dlp")
            .arg("-x")
            .arg("--audio-format")
            .arg("best")
            .arg("--embed-thumbnail")
            .arg("-o")
            .arg(&output_template)
            .arg(&url)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Download timed out."))?
    .map_err(|e| anyhow::anyhow!("Failed to run yt-dlp: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("{}", stderr.trim());
    }

    Ok(())
}

/// Download a skin from the webamp CDN and save it to the skins directory.
fn download_skin(
    tx: &mpsc::UnboundedSender<AppEvent>,
    md5: &str,
    filename: &str,
) {
    let tx = tx.clone();
    let md5 = md5.to_string();
    let filename = filename.to_string();
    tokio::spawn(async move {
        let result = download_skin_inner(&md5, &filename).await;
        let _ = tx.send(AppEvent::SkinDownloaded {
            md5,
            filename,
            result,
        });
    });
}

async fn download_skin_inner(md5: &str, filename: &str) -> anyhow::Result<()> {
    let url = format!("https://r2.webampskins.org/skins/{md5}.wsz");

    let client = reqwest::Client::new();
    let resp = client.get(&url).send().await?.error_for_status()?;
    let bytes = resp.bytes().await?;

    let skin_dir = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("~/.config"))
        .join("rustune")
        .join("skins");

    std::fs::create_dir_all(&skin_dir)?;
    let safe_filename = if filename.ends_with(".wsz") {
        filename.to_string()
    } else {
        format!("{filename}.wsz")
    };
    let skin_path = skin_dir.join(&safe_filename);
    std::fs::write(&skin_path, &bytes)?;

    Ok(())
}
