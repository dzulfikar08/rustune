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

fn main() -> Result<()> {
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        eprintln!("{info}");
        default_panic(info);
    }));

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let config = config::Config::load();

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
                BrowseAction::None => {}
            }
        }
        Mode::Input => {
            let action = app.handle_input_key(key);
            match action {
                InputAction::Search(_query) => {
                    // Switch to extractor source for search
                    if registry.first_available().is_some() {
                        app.active_source = SourceKind::Extractor("ytdlp".into());
                    }
                    dispatch_search(app, tx, registry);
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
                    // Populate with local skins
                    let local_skins = skin::WinampSkin::available_skins();
                    for p in &local_skins {
                        let filename = p.file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let display_name = p.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("unknown")
                            .to_string();
                        app.skin_entries.push(app::SkinEntry {
                            md5: String::new(),
                            filename,
                            display_name,
                            is_local: true,
                            nsfw: false,
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
                    fetch_skin_list(tx, 0, 20);
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
                        }
                        Err(e) => {
                            app.skin_browser_error = Some(format!("Failed to load skin: {e}"));
                        }
                    }
                }
                SkinBrowserAction::RequestFetch => {
                    app.skin_browser_loading = true;
                    fetch_skin_list(tx, app.skin_browser_offset, 20);
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

async fn fetch_skin_list_inner(
    offset: usize,
    limit: usize,
) -> anyhow::Result<(Vec<app::SkinEntry>, usize)> {
    let query = serde_json::json!({
        "query": format!(
            "{{ skins(first: {}, offset: {}) {{ nodes {{ md5 filename nsfw }} count }} }}",
            limit, offset
        )
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

    // Get local skin md5s for marking
    let local_skins = skin::WinampSkin::available_skins();
    let local_md5s: Vec<String> = local_skins
        .iter()
        .filter_map(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_lowercase())
        })
        .collect();

    let entries: Vec<app::SkinEntry> = skins_arr
        .into_iter()
        .filter(|s| !s["nsfw"].as_bool().unwrap_or(false))
        .map(|s| {
            let md5 = s["md5"].as_str().unwrap_or("").to_string();
            let raw_filename = s["filename"].as_str().unwrap_or("").to_string();
            let display_name = raw_filename.trim_end_matches(".wsz").trim_end_matches(".zip").to_string();
            let is_local = local_md5s.iter().any(|l| l == &md5.to_lowercase());
            app::SkinEntry {
                md5,
                filename: raw_filename,
                display_name,
                is_local,
                nsfw: false,
            }
        })
        .collect();

    Ok((entries, total_count))
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
