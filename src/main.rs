mod app;
mod cli;
mod command;
mod config;
mod controller;
mod eq_presets;
mod event;
mod extractor;
mod install;
mod library;
mod media;
mod net;
mod playback;
mod player;
mod playlist_io;
mod queue;
// Owned by the skin/gfx workstream, which carries its own lint fixes.
#[allow(dead_code, clippy::all)]
mod skin;
mod state;
#[allow(dead_code)] // Lock-free sample tap; consumed once the audio engine lands.
mod tap;
mod term_input;
mod theme;
mod ui;
// Renderer/engine contract; some fields are only consumed after integration.
#[allow(dead_code)]
mod view;

use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};

use cli::Cli;
use controller::Controller;
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
        Ok(Cli::Run { inputs }) => match run_tui(inputs) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("rustune: {e:#}");
                ExitCode::FAILURE
            }
        },
    }
}

fn restore_terminal() {
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
}

fn run_tui(inputs: Vec<String>) -> Result<()> {
    let (config, config_warning) = config::Config::load();
    let (state, state_warning) = SessionState::load(&config::state_path());
    let rt = runtime()?;

    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_panic(info);
    }));

    let result = rt.block_on(async move {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ctl = Controller::new(config, state, tx.clone());
        if let Some(warning) = config_warning.or(state_warning) {
            ctl.app.error(warning);
        }

        let terminal = ratatui::init();
        if let Err(e) = crossterm::execute!(std::io::stdout(), EnableMouseCapture) {
            restore_terminal();
            return Err(e.into());
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
        result
    });
    // Don't wait for in-flight blocking work (scans, downloads) on exit.
    rt.shutdown_timeout(Duration::from_millis(200));
    result
}
