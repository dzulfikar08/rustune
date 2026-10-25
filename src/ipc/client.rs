//! `rustune play|pause|status|volume|...`: the command-line remote, plus
//! handing `rustune <files>` to an instance that is already running.

use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::ExitCode;

use serde_json::Value;

use super::keys::{parse_key, text_keys};
use super::protocol::{
    check_reply, HitKind, LyricsReply, Query, QueueReply, Request, Screen, Search, SearchReply,
    SearchSource, Status, Then,
};
use super::transport::{self, Conn};
use crate::command::Command;
use crate::eq_presets;
use crate::view::{PlayStatus, RepeatMode};
use crate::vis::VisMode;

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
    List {
        json: bool,
    },
    /// Queue positions are 1-based here and checked against the queue.
    Jump(usize),
    Remove(usize),
    Move {
        from: usize,
        to: usize,
    },
    Search {
        search: Search,
        json: bool,
    },
    VisList,
    /// Installed skin name, museum md5 or `.wsz` path.
    Skin(String),
    Lyrics {
        json: bool,
    },
    Keys(Vec<String>),
    Screen {
        ansi: bool,
        json: bool,
    },
    /// A raw protocol line (`rustune send`).
    Raw(String),
}

/// Subcommand names `cli::parse` routes here.
pub const SUBCOMMANDS: &[&str] = &[
    "play", "pause", "toggle", "stop", "next", "prev", "status", "volume", "seek", "shuffle",
    "repeat", "eq", "queue", "watch", "quit", "list", "jump", "remove", "move", "clear", "search",
    "theme", "vis", "skin", "lyrics", "key", "screen", "send",
];

/// The flags in `args`, each one of `allowed`.
fn flags<'a>(name: &str, args: &'a [String], allowed: &[&str]) -> Result<Vec<&'a str>, String> {
    args.iter()
        .map(|a| {
            if allowed.contains(&a.as_str()) {
                Ok(a.as_str())
            } else {
                Err(format!(
                    "unexpected argument after '{name}': {a} (allowed: {})",
                    allowed.join(" ")
                ))
            }
        })
        .collect()
}

/// A 1-based position (queue index, search result, limit).
fn position(what: &str, arg: &str) -> Result<usize, String> {
    arg.parse::<usize>()
        .ok()
        .filter(|n| *n >= 1)
        .ok_or_else(|| format!("bad {what} '{arg}' (use 1, 2, 3, ...)"))
}

fn source_names() -> String {
    SearchSource::ALL.map(SearchSource::name).join(", ")
}

/// `search SOURCE QUERY... [--limit N] [--json] [--queue | --play N]`.
fn parse_search(args: &[String]) -> Result<Remote, String> {
    let mut args = args.iter();
    let source = args
        .next()
        .ok_or_else(|| format!("'search' needs a source ({}) and a query", source_names()))?;
    let source = SearchSource::from_name(source)
        .ok_or_else(|| format!("unknown source '{source}' ({})", source_names()))?;
    let (mut words, mut limit, mut json, mut then) = (Vec::new(), None, false, None);
    let value = |flag: &str, next: Option<&String>| {
        next.ok_or_else(|| format!("'{flag}' needs a number"))
            .and_then(|n| position(flag.trim_start_matches('-'), n))
    };
    let set_then = |then: &mut Option<Then>, t: Then| match then.replace(t) {
        Some(_) => Err("use only one of --queue and --play".to_string()),
        None => Ok(()),
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--limit" => limit = Some(value("--limit", args.next())?),
            "--json" => json = true,
            "--queue" => set_then(&mut then, Then::Queue)?,
            "--play" => set_then(&mut then, Then::Play(value("--play", args.next())?))?,
            flag if flag.starts_with("--") => {
                return Err(format!(
                    "unknown option for 'search': {flag} (--limit N, --json, --queue, --play N)"
                ))
            }
            word => words.push(word),
        }
    }
    if words.is_empty() {
        return Err("'search' needs a query".into());
    }
    Ok(Remote::Search {
        search: Search {
            source,
            query: words.join(" "),
            limit,
            then: then.unwrap_or_default(),
        },
        json,
    })
}

/// `key SPEC... [--text WORDS]...`, checked here for a clear error.
fn parse_keys(args: &[String]) -> Result<Remote, String> {
    let mut keys = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--text" {
            let text = args.next().ok_or("'--text' needs the text to type")?;
            keys.extend(text_keys(text));
        } else {
            parse_key(arg)?;
            keys.push(arg.clone());
        }
    }
    if keys.is_empty() {
        return Err("'key' needs keys (e.g. j, Enter, ctrl+k) or --text WORDS".into());
    }
    Ok(Remote::Keys(keys))
}

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
        "list" => Ok(Remote::List {
            json: !flags(name, args, &["--json"])?.is_empty(),
        }),
        "jump" => position("position", one_arg()?).map(Remote::Jump),
        "remove" => position("position", one_arg()?).map(Remote::Remove),
        "move" => match args {
            [from, to] => Ok(Remote::Move {
                from: position("position", from)?,
                to: position("position", to)?,
            }),
            _ => Err("'move' needs two positions: FROM TO".into()),
        },
        "clear" => send(Command::ClearQueue),
        "search" => parse_search(args),
        "theme" => match one_arg()?.to_ascii_lowercase().as_str() {
            "dark" => Ok(Remote::Send(Command::SetTheme("Dark".into()))),
            "light" => Ok(Remote::Send(Command::SetTheme("Light".into()))),
            "winamp" => Ok(Remote::Send(Command::SetTheme("Winamp".into()))),
            other => Err(format!("unknown theme '{other}' (dark, light, winamp)")),
        },
        "vis" => match one_arg()? {
            "next" => Ok(Remote::Send(Command::CycleVis)),
            "list" => Ok(Remote::VisList),
            mode => VisMode::from_name(mode)
                .map(|m| Remote::Send(Command::SetVis(m)))
                .ok_or_else(|| {
                    let names: Vec<&str> = VisMode::list().iter().map(|m| m.name()).collect();
                    format!(
                        "unknown visualizer '{mode}' (next, list, {})",
                        names.join(", ")
                    )
                }),
        },
        "skin" => Ok(Remote::Skin(one_arg()?.to_string())),
        "lyrics" => Ok(Remote::Lyrics {
            json: !flags(name, args, &["--json"])?.is_empty(),
        }),
        "key" => parse_keys(args),
        "screen" => {
            let set = flags(name, args, &["--ansi", "--json"])?;
            Ok(Remote::Screen {
                ansi: set.contains(&"--ansi"),
                json: set.contains(&"--json"),
            })
        }
        "send" => Ok(Remote::Raw(one_arg()?.to_string())),
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
        decode(value, "status")
    }

    fn query<T: serde::de::DeserializeOwned>(&mut self, query: Query) -> Result<T, Failure> {
        let value = self.request(&Request::Query(query))?;
        decode(value, "reply")
    }

    fn queue(&mut self) -> Result<QueueReply, Failure> {
        self.query(Query::Queue)
    }

    /// Send `line` as is; returns the raw reply line.
    fn raw(&mut self, line: &str) -> Result<String, Failure> {
        let conn = self.conn.get_mut();
        conn.write_all(line.trim().as_bytes())?;
        conn.write_all(b"\n")?;
        conn.flush()?;
        let reply = self
            .read()?
            .ok_or_else(|| "rustune closed the connection".to_string())?;
        Ok(reply.to_string())
    }
}

fn decode<T: serde::de::DeserializeOwned>(value: Value, what: &str) -> Result<T, Failure> {
    serde_json::from_value(value)
        .map_err(|e| Failure::Failed(format!("bad {what} from rustune: {e}")))
}

/// The queue entry at 1-based `n`, or a clear error.
fn entry_title(queue: &QueueReply, n: usize) -> Result<String, Failure> {
    match queue.items.get(n - 1) {
        Some(entry) => Ok(entry.title.clone()),
        None if queue.items.is_empty() => Err(Failure::Failed("the queue is empty".into())),
        None => Err(Failure::Failed(format!(
            "no item {n} in the queue (1-{})",
            queue.items.len()
        ))),
    }
}

fn print_json(value: &Value) {
    println!("{}", serde_json::to_string(value).unwrap_or_default());
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
        Remote::List { json: true } => {
            print_json(&client.request(&Request::Query(Query::Queue))?);
        }
        Remote::List { json: false } => print!("{}", format_queue(&client.queue()?)),
        Remote::Jump(n) => {
            let title = entry_title(&client.queue()?, n)?;
            client.command(Command::PlayIndex(n - 1))?;
            println!("Playing {n}. {title}");
        }
        Remote::Remove(n) => {
            let title = entry_title(&client.queue()?, n)?;
            client.command(Command::RemoveFromQueue(n - 1))?;
            println!("Removed {n}. {title}");
        }
        Remote::Move { from, to } => {
            let queue = client.queue()?;
            let title = entry_title(&queue, from)?;
            entry_title(&queue, to)?;
            client.command(Command::MoveInQueue {
                from: from - 1,
                to: to - 1,
            })?;
            println!("Moved {title} to {to}");
        }
        Remote::Search { search, json } => {
            let value = client.request(&Request::Query(Query::Search(search)))?;
            if json {
                print_json(&value);
            } else {
                print!("{}", format_search(&decode(value, "search reply")?));
            }
        }
        Remote::VisList => {
            let current = client.status()?.vis;
            for mode in VisMode::list() {
                let mark = if mode.name() == current { '*' } else { ' ' };
                println!("{mark} {}", mode.name());
            }
        }
        Remote::Skin(spec) => {
            // A local file: the running instance needs its absolute path.
            let spec = if Path::new(&spec).is_file() {
                absolutize(std::slice::from_ref(&spec)).remove(0)
            } else {
                spec
            };
            let value = client.request(&Request::Query(Query::Skin(spec)))?;
            println!(
                "Skin: {} ({})",
                value["skin"].as_str().unwrap_or("?"),
                value["path"].as_str().unwrap_or("?")
            );
        }
        Remote::Lyrics { json: true } => {
            print_json(&client.request(&Request::Query(Query::Lyrics))?);
        }
        Remote::Lyrics { json: false } => {
            print!("{}", format_lyrics(&client.query(Query::Lyrics)?));
        }
        Remote::Keys(keys) => {
            client.request(&Request::Query(Query::Keys(keys)))?;
        }
        Remote::Screen { ansi, json } => {
            let mut value = client.request(&Request::Query(Query::Screen))?;
            if json {
                if !ansi {
                    if let Some(map) = value.as_object_mut() {
                        map.remove("ansi");
                    }
                }
                print_json(&value);
            } else {
                let screen: Screen = decode(value, "screen")?;
                print!("{}", if ansi { &screen.ansi } else { &screen.text });
            }
        }
        Remote::Raw(line) => {
            let reply = client.raw(&line)?;
            println!("{reply}");
            check_reply(&reply)?;
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
    if !s.source.is_empty() {
        out.push_str(&format!(
            "View:    {}  source {}  theme {}  vis {}\n",
            s.view, s.source, s.theme, s.vis
        ));
    }
    if s.resolving {
        out.push_str("Loading: looking up the stream with yt-dlp\n");
    }
    if let Some(error) = &s.last_error {
        out.push_str(&format!("Error:   {error}\n"));
    }
    out
}

fn duration_suffix(duration: Option<u64>) -> String {
    duration.map_or_else(String::new, |d| format!("  [{}]", clock(d)))
}

/// `rustune list`: `>` marks the current track, `*` the cursor.
pub fn format_queue(queue: &QueueReply) -> String {
    if queue.items.is_empty() {
        return "(the queue is empty)\n".to_string();
    }
    let mut out = String::new();
    for e in &queue.items {
        let artist = e
            .artist
            .as_deref()
            .map_or_else(String::new, |a| format!(" - {a}"));
        out.push_str(&format!(
            "{}{}{:>3}. {}{artist}{}  ({})\n",
            if e.current { '>' } else { ' ' },
            if e.cursor { '*' } else { ' ' },
            e.index,
            e.title,
            duration_suffix(e.duration),
            e.source,
        ));
    }
    out.push_str("(> playing, * cursor)\n");
    out
}

/// `rustune search`: numbered results, then what was done with them.
pub fn format_search(reply: &SearchReply) -> String {
    let mut out = String::new();
    if let Some(opened) = &reply.opened {
        out.push_str(&format!("Opened: {opened}\n"));
    }
    if reply.results.is_empty() {
        out.push_str(&format!(
            "No results for '{}' in {}\n",
            reply.query,
            reply.source.name()
        ));
    }
    for hit in &reply.results {
        let artist = hit
            .artist
            .as_deref()
            .map_or_else(String::new, |a| format!(" - {a}"));
        let kind = match hit.kind {
            HitKind::Track => "",
            HitKind::Show => "  [show]",
            HitKind::Folder => "  [folder]",
        };
        let subtitle = hit
            .subtitle
            .as_deref()
            .map_or_else(String::new, |s| format!("  ({s})"));
        out.push_str(&format!(
            "{:>3}. {}{artist}{}{kind}{subtitle}\n",
            hit.index,
            hit.title,
            duration_suffix(hit.duration),
        ));
    }
    if let Some(n) = reply.queued {
        out.push_str(&format!("Queued {n} track(s)\n"));
    }
    if let Some(playing) = &reply.playing {
        out.push_str(&format!("Playing: {playing}\n"));
    }
    out
}

/// `rustune lyrics`: timed lines as `[m:ss] text`, else the plain text.
pub fn format_lyrics(reply: &LyricsReply) -> String {
    if !reply.found {
        let why = reply.message.as_deref().unwrap_or("none");
        return format!("No lyrics: {why}\n");
    }
    let mut out = format!("Lyrics: {}\n", reply.title);
    if reply.instrumental {
        out.push_str("(instrumental)\n");
    }
    if reply.synced.is_empty() {
        if let Some(plain) = &reply.plain {
            out.push_str(plain.trim_end());
            out.push('\n');
        }
    } else {
        for line in &reply.synced {
            out.push_str(&format!("[{}] {}\n", clock(line.time as u64), line.text));
        }
    }
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

    #[test]
    fn queue_editing_arguments() {
        assert_eq!(ok("list", &[]), Remote::List { json: false });
        assert_eq!(ok("list", &["--json"]), Remote::List { json: true });
        assert!(parse("list", &args(&["--yaml"])).is_err());
        assert_eq!(ok("jump", &["2"]), Remote::Jump(2));
        assert_eq!(ok("remove", &["1"]), Remote::Remove(1));
        assert_eq!(ok("move", &["1", "3"]), Remote::Move { from: 1, to: 3 });
        assert_eq!(ok("clear", &[]), Remote::Send(Command::ClearQueue));
        for bad in [
            ("jump", vec!["0"]),
            ("remove", vec!["x"]),
            ("move", vec!["1"]),
        ] {
            assert!(parse(bad.0, &args(&bad.1)).is_err(), "{bad:?}");
        }
        assert!(parse("jump", &args(&["0"]))
            .unwrap_err()
            .contains("1, 2, 3"));
    }

    #[test]
    fn search_arguments() {
        let search = |list: &[&str]| match ok("search", list) {
            Remote::Search { search, json } => (search, json),
            other => panic!("{other:?}"),
        };
        let (s, json) = search(&["local", "daft", "punk"]);
        assert_eq!(s.source, SearchSource::Local);
        assert_eq!(
            (s.query.as_str(), s.limit, s.then, json),
            ("daft punk", None, Then::Show, false)
        );
        let (s, json) = search(&["online", "lofi", "--limit", "5", "--json", "--play", "2"]);
        assert_eq!(
            (s.source, s.limit, s.then, json),
            (SearchSource::Online, Some(5), Then::Play(2), true)
        );
        let (s, _) = search(&["Podcasts", "--queue", "rust"]);
        assert_eq!((s.source, s.then), (SearchSource::Podcasts, Then::Queue));
        let err = |list: &[&str]| parse("search", &args(list)).unwrap_err();
        assert!(err(&[]).contains("needs a source"));
        assert!(err(&["mars", "x"]).contains("unknown source"));
        assert!(err(&["local"]).contains("needs a query"));
        assert!(err(&["local", "x", "--queue", "--play", "1"]).contains("only one"));
        assert!(err(&["local", "x", "--play", "0"]).contains("bad play"));
        assert!(err(&["local", "x", "--limit"]).contains("needs a number"));
        assert!(err(&["local", "x", "--loud"]).contains("unknown option"));
    }

    #[test]
    fn look_and_tui_arguments() {
        assert_eq!(
            ok("theme", &["Winamp"]),
            Remote::Send(Command::SetTheme("Winamp".into()))
        );
        assert!(parse("theme", &args(&["neon"])).is_err());
        assert_eq!(ok("vis", &["next"]), Remote::Send(Command::CycleVis));
        assert_eq!(ok("vis", &["list"]), Remote::VisList);
        assert_eq!(
            ok("vis", &["scope"]),
            Remote::Send(Command::SetVis(VisMode::Scope))
        );
        assert!(parse("vis", &args(&["lava"]))
            .unwrap_err()
            .contains("spectrogram"));
        assert_eq!(ok("skin", &["base-2.91"]), Remote::Skin("base-2.91".into()));
        assert_eq!(ok("lyrics", &["--json"]), Remote::Lyrics { json: true });
        assert_eq!(
            ok("screen", &["--ansi", "--json"]),
            Remote::Screen {
                ansi: true,
                json: true
            }
        );
        assert_eq!(
            ok("send", &[r#"{"get":"queue"}"#]),
            Remote::Raw(r#"{"get":"queue"}"#.into())
        );
        assert_eq!(
            ok("key", &["j", "ctrl+k", "--text", "a b", "Enter"]),
            Remote::Keys(args(&["j", "ctrl+k", "a", "Space", "b", "Enter"]))
        );
        assert!(parse("key", &args(&[])).is_err());
        assert!(parse("key", &args(&["Enterr"]))
            .unwrap_err()
            .contains("bad key"));
        assert!(parse("key", &args(&["--text"])).is_err());
    }

    #[test]
    fn listings_are_readable() {
        use crate::ipc::protocol::{Hit, LyricLine, QueueEntry};
        let entry = |index: usize, current: bool, cursor: bool| QueueEntry {
            index,
            title: format!("Song {index}"),
            artist: (index == 1).then(|| "Band".to_string()),
            album: None,
            duration: Some(75),
            source: "local".into(),
            id: format!("/m/{index}.flac"),
            current,
            cursor,
        };
        let queue = QueueReply {
            items: vec![entry(1, true, false), entry(2, false, true)],
        };
        let text = format_queue(&queue);
        assert!(
            text.contains(">   1. Song 1 - Band  [1:15]  (local)"),
            "{text}"
        );
        assert!(text.contains(" *  2. Song 2"), "{text}");
        assert_eq!(
            format_queue(&QueueReply::default()),
            "(the queue is empty)\n"
        );

        let mut hit = Hit::track(
            1,
            &crate::media::MediaItem {
                id: "abc".into(),
                title: "Video".into(),
                artist: None,
                album: None,
                duration: Some(61),
                subtitle: Some("Channel".into()),
                source: crate::media::SourceKind::Extractor("ytdlp".into()),
            },
        );
        let reply = SearchReply {
            source: SearchSource::Online,
            query: "v".into(),
            results: vec![hit.clone()],
            queued: None,
            opened: None,
            playing: Some("Video".into()),
        };
        let text = format_search(&reply);
        assert!(text.contains("  1. Video  [1:01]  (Channel)\n"), "{text}");
        assert!(text.ends_with("Playing: Video\n"));
        hit.kind = HitKind::Show;
        let shows = SearchReply {
            results: vec![hit],
            playing: None,
            ..reply
        };
        assert!(format_search(&shows).contains("[show]"));

        let lyrics = LyricsReply {
            found: true,
            title: "Song".into(),
            synced: vec![LyricLine {
                time: 62.5,
                text: "Hello".into(),
            }],
            ..LyricsReply::default()
        };
        assert_eq!(format_lyrics(&lyrics), "Lyrics: Song\n[1:02] Hello\n");
        let none = LyricsReply::none("Song", "no lyrics found");
        assert_eq!(format_lyrics(&none), "No lyrics: no lyrics found\n");
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
