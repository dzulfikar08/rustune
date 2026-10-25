mod app;
mod audio;
mod browse;
mod cli;
mod command;
mod config;
mod controller;
mod eq_presets;
mod event;
mod extractor;
mod gfx;
mod install;
mod ipc;
mod keymap;
mod library;
mod media;
mod mediactl;
mod net;
mod playback;
mod playlist_io;
mod providers;
mod queue;
mod report;
mod skin;
mod skin_browser;
mod skin_cache;
mod state;
mod tap;
mod term_input;
mod theme;
mod ui;
mod view;
mod vis;
mod winamp;

use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};

use cli::Cli;
use config::Config;
use controller::Controller;
use gfx::GfxPainter;
use playback::Playback;
use state::SessionState;
use term_input::InputThread;

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

fn main() -> ExitCode {
    let mut args = std::env::args();
    let program = args.next().unwrap_or_else(|| "rustune".into());
    match cli::parse(args) {
        Err(msg) => {
            eprintln!("rustune: {msg}\nRun '{program} --help' for usage.");
            ExitCode::from(2)
        }
        Ok(Cli::Help) => {
            println!("{}", cli::help_text(&program));
            ExitCode::SUCCESS
        }
        Ok(Cli::Version) => {
            println!("rustune {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Cli::Doctor) => match runtime() {
            Ok(rt) => {
                rt.block_on(cli::doctor());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("rustune: {e}");
                ExitCode::FAILURE
            }
        },
        Ok(Cli::Remote(remote)) => ipc::client::run(remote),
        // macOS media keys need the main thread's run loop (`mediactl::host`).
        Ok(Cli::Daemon { inputs }) => match mediactl::host(move || ipc::daemon::run(inputs)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("rustune: {e:#}");
                ExitCode::FAILURE
            }
        },
        Ok(Cli::Run { inputs }) => {
            // Another instance is running: queue the items there.
            let handed_off = if inputs.is_empty() {
                None
            } else {
                ipc::client::hand_off(&inputs)
            };
            if let Some(code) = handed_off {
                return code;
            }
            match mediactl::host(move || run_tui(inputs)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("rustune: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

fn restore_terminal() {
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
}

/// Pick the graphics protocol for Winamp skins. With `graphics = "auto"`
/// and the Winamp theme active, ask the terminal (needs raw mode and must
/// run before the input thread starts).
fn graphics_painter(config: &Config, winamp: bool) -> GfxPainter {
    let protocol = gfx::detect(&config.graphics);
    let auto = matches!(config.graphics.trim(), "" | "auto");
    if !(auto && winamp) {
        return GfxPainter::new(protocol);
    }
    let probe = gfx::probe(Duration::from_millis(150));
    let cell = probe.cell.or_else(gfx::query_cell_size);
    GfxPainter::with_cell_size(probe.protocol(protocol), cell)
}

fn run_tui(inputs: Vec<String>) -> Result<()> {
    let (config, config_warning) = Config::load();
    let (state, state_warning) = SessionState::load(&config::state_path());
    let winamp = state.theme.as_deref().unwrap_or(&config.theme) == "Winamp";
    let rt = runtime()?;

    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_panic(info);
    }));

    let result = rt.block_on(async move {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        // Open the audio device before the TUI owns the screen: some audio
        // stacks print diagnostics while probing devices.
        let playback = Playback::new(tx.clone());

        let terminal = ratatui::init();
        if let Err(e) = crossterm::execute!(std::io::stdout(), EnableMouseCapture) {
            restore_terminal();
            return Err(e.into());
        }
        let painter = graphics_painter(&config, winamp);
        let mut ctl = Controller::new(config, state, tx.clone(), playback, painter);
        let media_controls = ctl.app.config.media_controls;
        ctl.observers
            .extend(mediactl::start(media_controls, tx.clone()));
        // Remote control; without it (e.g. a second instance) the TUI still runs.
        let remote = match ipc::IpcServer::start(ipc::endpoint(), tx.clone()) {
            Ok(server) => {
                ctl.observers.push(server.observer());
                Some(server)
            }
            Err(e) => {
                ctl.app.error(format!("Remote control unavailable: {e}"));
                None
            }
        };
        if let Some(warning) = config_warning.or(state_warning) {
            ctl.app.error(warning);
        }
        let input = match InputThread::spawn(tx) {
            Ok(input) => input,
            Err(e) => {
                restore_terminal();
                return Err(e.into());
            }
        };

        ctl.start(inputs);
        let result = controller::run(terminal, &mut ctl, rx, &input).await;
        restore_terminal();
        ctl.shutdown();
        drop(remote);
        result
    });
    // Don't wait for in-flight blocking work (scans, downloads) on exit.
    rt.shutdown_timeout(Duration::from_millis(200));
    result
}
