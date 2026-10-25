//! `rustune play|pause|status|volume|...`: the command-line remote, plus
//! handing `rustune <files>` to an instance that is already running.

use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::ExitCode;

use super::protocol::{check_reply, Request, Status};
use super::transport::{self, Conn};
use crate::command::Command;
use crate::eq_presets;
use crate::view::{PlayStatus, RepeatMode};

/// What a remote subcommand asks the running instance to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Remote {
    Send(Command),
    /// Apply `preset` (if any), then make sure the EQ is `on`/off.
    Eq {
        preset: Option<String>,
        on: bool,
    },
    Status {
        json: bool,
    },
    Watch,
}

/// Subcommand names `cli::parse` routes here.
pub const SUBCOMMANDS: &[&str] = &[
    "play", "pause", "toggle", "stop", "next", "prev", "status", "volume", "seek", "shuffle",
    "repeat", "eq", "queue", "watch", "quit",
];

/// Parse `rustune <name> <args>...` for a name in [`SUBCOMMANDS`].
pub fn parse(name: &str, args: &[String]) -> Result<Remote, String> {
    let no_args = |remote: Remote| match args.first() {
        None => Ok(remote),
        Some(extra) => Err(format!("unexpected argument after '{name}': {extra}")),
    };
    let send = |cmd: Command| no_args(Remote::Send(cmd));
    let one_arg = || match args {
        [arg] => Ok(arg.as_str()),
        [] => Err(format!("'{name}' needs an argument")),
        [_, extra, ..] => Err(format!("unexpected argument after '{name}': {extra}")),
    };
    match name {
        "play" => send(Command::Play),
        "pause" => send(Command::Pause),
        "toggle" => send(Command::TogglePause),
        "stop" => send(Command::Stop),
        "next" => send(Command::Next),
        "prev" => send(Command::Prev),
        "shuffle" => send(Command::ToggleShuffle),
        "quit" => send(Command::Quit),
        "watch" => no_args(Remote::Watch),
        "status" => match args {
            [] => Ok(Remote::Status { json: false }),
            [flag] if flag == "--json" => Ok(Remote::Status { json: true }),
            [extra, ..] => Err(format!("unexpected argument after 'status': {extra}")),
        },
        "volume" => parse_volume(one_arg()?).map(Remote::Send),
        "seek" => crate::controller::parse_seek(one_arg()?).map(Remote::Send),
        "repeat" => match args {
            [] => Ok(Remote::Send(Command::CycleRepeat)),
            _ => parse_repeat(one_arg()?).map(|m| Remote::Send(Command::SetRepeat(m))),
        },
        "eq" if args.is_empty() => Err("'eq' needs a preset, on, off or toggle".into()),
        "eq" => parse_eq(&args.join(" ")),
        "queue" if args.is_empty() => Err("'queue' needs files, directories or URLs".into()),
        "queue" => Ok(Remote::Send(Command::Enqueue(args.to_vec()))),
        _ => Err(format!("unknown command: {name}")),
    }
}

/// `50` / `50%` → absolute; `+5` / `-5` → relative (percent).
pub fn parse_volume(arg: &str) -> Result<Command, String> {
    let bad = || format!("bad volume '{arg}' (use 0-100, +N or -N)");
    let num = |s: &str| {
        s.trim_end_matches('%')
            .parse::<f32>()
            .ok()
            .filter(|v| v.is_finite() && (0.0..=100.0).contains(v))
            .ok_or_else(bad)
    };
    if let Some(delta) = arg.strip_prefix('+') {
        Ok(Command::VolumeBy(num(delta)? / 100.0))
    } else if let Some(delta) = arg.strip_prefix('-') {
        Ok(Command::VolumeBy(-num(delta)? / 100.0))
    } else {
        Ok(Command::SetVolume(num(arg)? / 100.0))
    }
}

fn parse_repeat(arg: &str) -> Result<RepeatMode, String> {
    match arg {
        "off" => Ok(RepeatMode::Off),
        "all" => Ok(RepeatMode::All),
        "one" => Ok(RepeatMode::One),
        _ => Err(format!("unknown repeat mode '{arg}' (off, all, one)")),
    }
}

fn parse_eq(arg: &str) -> Result<Remote, String> {
    match arg {
        "on" => Ok(Remote::Eq {
            preset: None,
            on: true,
        }),
        "off" => Ok(Remote::Eq {
            preset: None,
            on: false,
        }),
        "toggle" => Ok(Remote::Send(Command::ToggleEq)),
        name => match eq_presets::PRESETS
            .iter()
            .find(|(preset, _)| preset.eq_ignore_ascii_case(name.trim()))
        {
            Some((preset, _)) => Ok(Remote::Eq {
                preset: Some((*preset).to_string()),
                on: true,
            }),
            None => {
                let names: Vec<&str> = eq_presets::PRESETS.iter().map(|(n, _)| *n).collect();
                Err(format!(
                    "unknown EQ preset '{name}' (on, off, toggle, {})",
                    names.join(", ")
                ))
            }
        },
    }
}

/// Make path arguments absolute: the running instance resolves them from
/// its own working directory.
fn absolutize(inputs: &[String]) -> Vec<String> {
    inputs
        .iter()
        .map(|input| {
            if input.contains("://") {
                return input.clone();
            }
            let path = crate::config::expand_tilde(input);
            std::path::absolute(&path)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| input.clone())
        })
        .collect()
}

enum Failure {
    /// Nothing listens on the endpoint.
    NotRunning,
    Failed(String),
}

impl From<io::Error> for Failure {
    fn from(e: io::Error) -> Self {
        Self::Failed(format!("connection to rustune failed: {e}"))
    }
}

impl From<String> for Failure {
    fn from(e: String) -> Self {
        Self::Failed(e)
    }
}

struct Client {
    conn: BufReader<Conn>,
    line: String,
}

impl Client {
    fn connect(endpoint: &Path) -> Result<Self, Failure> {
        let conn = transport::connect(endpoint).map_err(|_| Failure::NotRunning)?;
        Ok(Self {
            conn: BufReader::new(conn),
            line: String::new(),
        })
    }

    fn send(&mut self, req: &Request) -> Result<(), Failure> {
        let mut line = req.encode();
        line.push('\n');
        let conn = self.conn.get_mut();
        conn.write_all(line.as_bytes())?;
        conn.flush()?;
        Ok(())
    }

    /// Next reply line; `None` when the instance hung up.
    fn read(&mut self) -> Result<Option<&str>, Failure> {
        self.line.clear();
        if self.conn.read_line(&mut self.line)? == 0 {
            return Ok(None);
        }
        Ok(Some(self.line.trim_end()))
    }

    fn request(&mut self, req: &Request) -> Result<serde_json::Value, Failure> {
        self.send(req)?;
        let line = self
            .read()?
            .ok_or_else(|| "rustune closed the connection".to_string())?;
        Ok(check_reply(line)?)
    }

    fn command(&mut self, cmd: Command) -> Result<(), Failure> {
        self.request(&Request::Command(cmd)).map(drop)
    }

    fn status(&mut self) -> Result<Status, Failure> {
        let value = self.request(&Request::Status)?;
        serde_json::from_value(value)
            .map_err(|e| Failure::Failed(format!("bad status from rustune: {e}")))
    }
}

fn execute(endpoint: &Path, remote: Remote) -> Result<(), Failure> {
    let mut client = Client::connect(endpoint)?;
    match remote {
        Remote::Send(Command::Enqueue(inputs)) => {
            client.command(Command::Enqueue(absolutize(&inputs)))?;
        }
        Remote::Send(cmd) => client.command(cmd)?,
        Remote::Eq { preset, on } => {
            if let Some(name) = preset {
                client.command(Command::EqPreset(name))?;
            }
            if client.status()?.eq_enabled != on {
                client.command(Command::ToggleEq)?;
            }
        }
        Remote::Status { json } => {
            let status = client.status()?;
            if json {
                println!("{}", serde_json::to_string(&status).unwrap_or_default());
            } else {
                print!("{}", format_status(&status));
            }
        }
        Remote::Watch => {
            client.send(&Request::Subscribe)?;
            let mut out = io::stdout().lock();
            while let Some(line) = client.read()? {
                check_reply(line)?;
                if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// Run a remote subcommand against the running instance.
pub fn run(remote: Remote) -> ExitCode {
    let endpoint = transport::endpoint();
    match execute(&endpoint, remote) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::NotRunning) => {
            eprintln!(
                "rustune: no running instance ({} not reachable); start one with \
                 'rustune' or 'rustune --daemon'",
                endpoint.display()
            );
            ExitCode::FAILURE
        }
        Err(Failure::Failed(msg)) => {
            eprintln!("rustune: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// `rustune <files>` while an instance runs: queue them there instead of
/// starting a second player. `None` = nothing is running.
pub fn hand_off(inputs: &[String]) -> Option<ExitCode> {
    let endpoint = transport::endpoint();
    let mut client = Client::connect(&endpoint).ok()?;
    let result = client.command(Command::Enqueue(absolutize(inputs)));
    Some(match result {
        Ok(()) => {
            println!(
                "rustune: queued {} item(s) in the running instance",
                inputs.len()
            );
            ExitCode::SUCCESS
        }
        Err(Failure::NotRunning) => return None,
        Err(Failure::Failed(msg)) => {
            eprintln!("rustune: {msg}");
            ExitCode::FAILURE
        }
    })
}

fn clock(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Human-readable `rustune status`.
pub fn format_status(s: &Status) -> String {
    let state = match s.status {
        PlayStatus::Playing => "Playing",
        PlayStatus::Paused => "Paused",
        PlayStatus::Stopped => "Stopped",
    };
    let mut out = String::new();
    let track = match (&s.artist, s.title.is_empty()) {
        (_, true) => "(nothing queued)".to_string(),
        (Some(artist), false) => format!("{artist} - {}", s.title),
        (None, false) => s.title.clone(),
    };
    out.push_str(&format!("{state}: {track}\n"));
    if let Some(album) = &s.album {
        out.push_str(&format!("Album:   {album}\n"));
    }
    let total = s.duration.map_or_else(|| "--:--".to_string(), clock);
    out.push_str(&format!("Time:    {} / {total}\n", clock(s.elapsed)));
    let on = |b: bool| if b { "on" } else { "off" };
    let repeat = match s.repeat {
        RepeatMode::Off => "off",
        RepeatMode::All => "all",
        RepeatMode::One => "one",
    };
    out.push_str(&format!(
        "Volume:  {}%  balance {}  shuffle {}  repeat {repeat}  eq {}\n",
        s.volume,
        s.balance,
        on(s.shuffle),
        on(s.eq_enabled)
    ));
    let position = s
        .queue_index
        .map_or_else(|| "-".to_string(), |i| (i + 1).to_string());
    out.push_str(&format!("Queue:   {position}/{}\n", s.queue_length));
    out
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn ok(name: &str, list: &[&str]) -> Remote {
        parse(name, &args(list)).unwrap()
    }

    #[test]
    fn volume_arguments() {
        assert_eq!(parse_volume("40"), Ok(Command::SetVolume(0.4)));
        assert_eq!(parse_volume("100%"), Ok(Command::SetVolume(1.0)));
        assert_eq!(parse_volume("0"), Ok(Command::SetVolume(0.0)));
        assert_eq!(parse_volume("+5"), Ok(Command::VolumeBy(0.05)));
        assert_eq!(parse_volume("-10"), Ok(Command::VolumeBy(-0.1)));
        for bad in ["101", "-101", "loud", "", "+", "nan", "inf"] {
            assert!(parse_volume(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn seek_arguments() {
        let seek = |s: &str| ok("seek", &[s]);
        assert_eq!(
            seek("90"),
            Remote::Send(Command::SeekTo(Duration::from_secs(90)))
        );
        assert_eq!(
            seek("1:30"),
            Remote::Send(Command::SeekTo(Duration::from_secs(90)))
        );
        assert_eq!(seek("+15"), Remote::Send(Command::SeekBy(15)));
        assert_eq!(seek("-15"), Remote::Send(Command::SeekBy(-15)));
        assert!(parse("seek", &args(&["1:xx"])).is_err());
        assert!(parse("seek", &args(&[])).is_err());
        assert!(parse("seek", &args(&["1", "2"])).is_err());
    }

    #[test]
    fn subcommands() {
        assert_eq!(ok("toggle", &[]), Remote::Send(Command::TogglePause));
        assert_eq!(ok("prev", &[]), Remote::Send(Command::Prev));
        assert!(parse("next", &args(&["x"])).is_err());
        assert_eq!(ok("status", &[]), Remote::Status { json: false });
        assert_eq!(ok("status", &["--json"]), Remote::Status { json: true });
        assert!(parse("status", &args(&["--yaml"])).is_err());
        assert_eq!(ok("repeat", &[]), Remote::Send(Command::CycleRepeat));
        assert_eq!(
            ok("repeat", &["one"]),
            Remote::Send(Command::SetRepeat(RepeatMode::One))
        );
        assert!(parse("repeat", &args(&["twice"])).is_err());
        assert_eq!(ok("watch", &[]), Remote::Watch);
        assert_eq!(
            ok("queue", &["a.mp3", "https://x/s"]),
            Remote::Send(Command::Enqueue(args(&["a.mp3", "https://x/s"])))
        );
        assert!(parse("queue", &args(&[])).is_err());
        for name in SUBCOMMANDS {
            assert!(
                !parse(name, &args(&["1"])).is_err_and(|e| e.starts_with("unknown command")),
                "{name}"
            );
        }
    }

    #[test]
    fn eq_arguments() {
        assert_eq!(
            ok("eq", &["off"]),
            Remote::Eq {
                preset: None,
                on: false
            }
        );
        assert_eq!(ok("eq", &["toggle"]), Remote::Send(Command::ToggleEq));
        assert_eq!(
            ok("eq", &["soft", "rock"]),
            Remote::Eq {
                preset: Some("Soft Rock".into()),
                on: true
            }
        );
        let err = parse("eq", &args(&["polka"])).unwrap_err();
        assert!(err.contains("Classical"), "{err}");
    }

    #[test]
    fn paths_become_absolute_urls_stay() {
        let out = absolutize(&args(&[
            "song.mp3",
            "https://r.example/live",
            "/abs/x.flac",
        ]));
        assert!(Path::new(&out[0]).is_absolute(), "{}", out[0]);
        assert!(out[0].ends_with("song.mp3"));
        assert_eq!(out[1], "https://r.example/live");
        assert!(Path::new(&out[2]).is_absolute());
        assert!(out[2].ends_with("x.flac"));
    }

    #[test]
    fn human_status() {
        let status = Status {
            status: PlayStatus::Playing,
            title: "Song".into(),
            artist: Some("Band".into()),
            album: Some("LP".into()),
            elapsed: 83,
            duration: Some(225),
            volume: 80,
            repeat: RepeatMode::All,
            queue_length: 10,
            queue_index: Some(1),
            ..Status::default()
        };
        let text = format_status(&status);
        assert!(text.starts_with("Playing: Band - Song\n"), "{text}");
        assert!(text.contains("Album:   LP"));
        assert!(text.contains("1:23 / 3:45"));
        assert!(text.contains("Volume:  80%"));
        assert!(text.contains("repeat all"));
        assert!(text.contains("Queue:   2/10"));
        let empty = format_status(&Status::default());
        assert!(empty.starts_with("Stopped: (nothing queued)"), "{empty}");
        assert!(empty.contains("--:--"));
    }

    #[cfg(unix)]
    #[test]
    fn missing_instance_is_reported() {
        let path = std::env::temp_dir().join(format!("rt-ipc-none-{}.sock", std::process::id()));
        assert!(matches!(
            execute(&path, Remote::Send(Command::Next)),
            Err(Failure::NotRunning)
        ));
    }
}
