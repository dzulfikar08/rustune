mod app;
mod event;
mod player;
mod ui;
mod youtube;

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, KeyEventKind};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

use app::{App, BrowseAction, InputAction, Mode, Status};
use event::AppEvent;

fn main() -> Result<()> {
    // Install panic hook to restore terminal on panic
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
    // Startup checks
    if let Err(e) = youtube::check_ytdlp().await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
    if let Err(e) = player::check_mpv().await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }

    let terminal = ratatui::init();
    let result = run(terminal).await;
    ratatui::restore();
    result
}

async fn run(mut terminal: DefaultTerminal) -> Result<()> {
    let mut app = App::new();

    // Channel for async events (search results, playback updates)
    let (tx_app, mut rx_app) = mpsc::unbounded_channel::<AppEvent>();

    // Bridge crossterm events into a tokio channel
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
                if let Event::Key(key) = term_event {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    handle_key(&mut app, key, &tx_app);
                }
            }
            Some(app_event) = rx_app.recv() => {
                handle_app_event(&mut app, app_event, &tx_app);
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

fn handle_key(app: &mut App, key: crossterm::event::KeyEvent, tx: &mpsc::UnboundedSender<AppEvent>) {
    match app.mode {
        Mode::Browse => {
            let action = app.handle_browse_key(key);
            match action {
                BrowseAction::Play(video_id, title) => {
                    app.kill_mpv();
                    app.status = Status::Loading("Loading stream...".into());
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let result = youtube::get_stream_url(&video_id).await;
                        let _ = tx.send(AppEvent::StreamUrlReady(result.map(|url| (url, title))));
                    });
                }
                BrowseAction::NextPage => {
                    if !app.results.is_empty() {
                        app.page += 1;
                        let query = app.input_history.last().cloned().unwrap_or_default();
                        search(app, query, tx);
                    }
                }
                BrowseAction::PrevPage => {
                    if app.page > 0 {
                        app.page -= 1;
                        let query = app.input_history.last().cloned().unwrap_or_default();
                        search(app, query, tx);
                    }
                }
                BrowseAction::TogglePause => {
                    if let Some(ref playback) = app.playback {
                        let new_paused = !playback.paused;
                        if let Some(ref mut pb) = app.playback {
                            pb.paused = new_paused;
                        }
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            let _ = player::set_pause(new_paused).await;
                        });
                    }
                }
                BrowseAction::None => {}
            }
        }
        Mode::Input => {
            let action = app.handle_input_key(key);
            match action {
                InputAction::Search(query) => {
                    search(app, query, tx);
                }
                InputAction::None => {}
            }
        }
    }
}

fn search(app: &mut App, query: String, tx: &mpsc::UnboundedSender<AppEvent>) {
    app.status = Status::Searching("Searching...".into());
    app.results.clear();
    app.list_state.select(Some(0));
    let page = app.page;
    let tx = tx.clone();
    tokio::spawn(async move {
        let result = youtube::search(&query, page).await;
        let _ = tx.send(AppEvent::SearchComplete(result));
    });
}

fn handle_app_event(app: &mut App, event: AppEvent, tx: &mpsc::UnboundedSender<AppEvent>) {
    match event {
        AppEvent::SearchComplete(result) => {
            match result {
                Ok(results) => {
                    app.results = results;
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
        AppEvent::StreamUrlReady(result) => {
            match result {
                Ok((url, title)) => {
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
    }
}
