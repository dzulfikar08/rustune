//! `rustune --daemon`: the controller, engine and queue without a terminal,
//! driven over IPC. Session state is saved like the TUI does; SIGINT /
//! SIGTERM (Ctrl+C on Windows) save and exit cleanly.

use std::time::Duration;

use anyhow::{Context, Result};

use super::IpcServer;
use crate::app::{App, Status};
use crate::config::{self, Config};
use crate::controller::{self, Controller};
use crate::gfx::{GfxPainter, Protocol};
use crate::playback::Playback;
use crate::state::SessionState;

/// Resolves on the first SIGINT/SIGTERM (Ctrl+C on Windows).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
        ) {
            (Ok(mut int), Ok(mut term)) => {
                tokio::select! {
                    _ = int.recv() => {}
                    _ = term.recv() => {}
                }
            }
            // Handlers not installed: the default action still ends the
            // process, just without the final save.
            _ => std::future::pending().await,
        }
    }
    #[cfg(not(unix))]
    {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Print player errors (bad files, no audio device) to stderr: the daemon
/// has no screen to show them on.
fn error_logger() -> Box<dyn FnMut(&App)> {
    let mut last = String::new();
    Box::new(move |app: &App| match &app.status {
        Status::Error(msg) if *msg != last => {
            eprintln!("rustune: {msg}");
            last.clone_from(msg);
        }
        Status::Error(_) => {}
        _ => last.clear(),
    })
}

pub fn run(inputs: Vec<String>) -> Result<()> {
    let (config, config_warning) = Config::load();
    let (state, state_warning) = SessionState::load(&config::state_path());
    for warning in [config_warning, state_warning].into_iter().flatten() {
        eprintln!("rustune: {warning}");
    }
    let rt = crate::runtime()?;
    let result = rt.block_on(async move {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let endpoint = super::endpoint();
        let server = IpcServer::start(endpoint.clone(), tx.clone())
            .with_context(|| format!("cannot listen on {}", endpoint.display()))?;
        let playback = Playback::new(tx.clone());
        let painter = GfxPainter::with_cell_size(Protocol::HalfBlock, None);
        let mut ctl = Controller::new(config, state, tx, playback, painter);
        ctl.observers.push(server.observer());
        ctl.observers.push(error_logger());
        ctl.start_headless(inputs);
        eprintln!(
            "rustune: daemon listening on {}",
            server.endpoint().display()
        );
        controller::run_headless(&mut ctl, rx, shutdown_signal()).await;
        ctl.shutdown();
        drop(server);
        Ok::<(), anyhow::Error>(())
    });
    rt.shutdown_timeout(Duration::from_millis(200));
    result
}
