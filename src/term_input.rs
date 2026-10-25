//! Terminal input on a dedicated OS thread (blocking crossterm poll/read),
//! feeding the app event channel so no runtime thread ever blocks on stdin.
//! The reader can be parked while a child process owns the terminal.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use crossterm::event::{Event, KeyEventKind, MouseEventKind};
use tokio::sync::mpsc::UnboundedSender;

use crate::event::AppEvent;

/// How long one `poll` blocks before re-checking the pause flag. Only bounds
/// pause latency; there is no busy loop.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Default)]
struct GateState {
    paused: bool,
    parked: bool,
}

#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    cv: Condvar,
}

impl Gate {
    fn lock(&self) -> MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct InputThread {
    gate: Arc<Gate>,
}

/// Drop events nobody consumes (mouse motion without a button, key
/// releases, focus changes) before they cost a wake-up and a redraw.
fn wanted(event: &Event) -> bool {
    match event {
        Event::Key(key) => key.kind != KeyEventKind::Release,
        Event::Mouse(mouse) => !matches!(mouse.kind, MouseEventKind::Moved),
        Event::Resize(..) | Event::Paste(_) => true,
        Event::FocusGained | Event::FocusLost => false,
    }
}

impl InputThread {
    pub fn spawn(tx: UnboundedSender<AppEvent>) -> std::io::Result<Self> {
        let gate = Arc::new(Gate::default());
        let worker_gate = gate.clone();
        thread::Builder::new()
            .name("rustune-input".into())
            .spawn(move || run(&worker_gate, &tx))?;
        Ok(Self { gate })
    }

    /// Stop reading stdin; returns once the reader is parked (or after 2 s).
    pub fn pause(&self) {
        let mut st = self.gate.lock();
        st.paused = true;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !st.parked {
            let now = std::time::Instant::now();
            if now >= deadline {
                break;
            }
            st = self
                .gate
                .cv
                .wait_timeout(st, deadline - now)
                .map(|(g, _)| g)
                .unwrap_or_else(|e| e.into_inner().0);
        }
    }

    pub fn resume(&self) {
        self.gate.lock().paused = false;
        self.gate.cv.notify_all();
    }
}

fn run(gate: &Gate, tx: &UnboundedSender<AppEvent>) {
    loop {
        {
            let mut st = gate.lock();
            while st.paused {
                st.parked = true;
                gate.cv.notify_all();
                st = gate.cv.wait(st).unwrap_or_else(|e| e.into_inner());
            }
            st.parked = false;
        }
        if tx.is_closed() {
            return;
        }
        match crossterm::event::poll(POLL_INTERVAL) {
            Ok(true) => match crossterm::event::read() {
                Ok(event) if wanted(&event) => {
                    if tx.send(AppEvent::Term(event)).is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                Err(_) => return,
            },
            Ok(false) => {}
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent};

    fn mouse(kind: MouseEventKind) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn filters_noise_events() {
        let mut release = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(wanted(&Event::Key(release)));
        release.kind = KeyEventKind::Release;
        assert!(!wanted(&Event::Key(release)));
        assert!(!wanted(&mouse(MouseEventKind::Moved)));
        assert!(wanted(&mouse(MouseEventKind::Drag(MouseButton::Left))));
        assert!(wanted(&mouse(MouseEventKind::Down(MouseButton::Left))));
        assert!(wanted(&Event::Resize(80, 24)));
        assert!(!wanted(&Event::FocusLost));
    }
}
