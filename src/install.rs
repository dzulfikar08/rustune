//! Optional external tools (mpv for streams, yt-dlp for online search):
//! background availability probing and the onboarding installer.

use std::path::Path;
use std::process::Stdio;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DepStatus {
    #[default]
    Checking,
    Found(String),
    Missing,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Deps {
    pub mpv: DepStatus,
    pub ytdlp: DepStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dep {
    Mpv,
    Ytdlp,
}

impl Dep {
    /// Onboarding list order.
    pub const ALL: [Dep; 2] = [Dep::Mpv, Dep::Ytdlp];

    pub fn binary(self) -> &'static str {
        match self {
            Dep::Mpv => "mpv",
            Dep::Ytdlp => "yt-dlp",
        }
    }

    pub fn homepage(self) -> &'static str {
        match self {
            Dep::Mpv => "https://mpv.io",
            Dep::Ytdlp => "https://github.com/yt-dlp/yt-dlp",
        }
    }
}

/// First non-empty line of `--version` output, trimmed to "name version".
fn version_line(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout);
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    line.split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

async fn probe_one(dep: Dep) -> DepStatus {
    let output = tokio::process::Command::new(dep.binary())
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(std::time::Duration::from_secs(10), output).await {
        Ok(Ok(out)) if out.status.success() => DepStatus::Found(version_line(&out.stdout)),
        _ => DepStatus::Missing,
    }
}

/// Probe both tools concurrently without blocking the runtime thread.
pub async fn probe_deps() -> Deps {
    let (mpv, ytdlp) = tokio::join!(probe_one(Dep::Mpv), probe_one(Dep::Ytdlp));
    Deps { mpv, ytdlp }
}

/// Is `program` an executable file on `$PATH`? (No subprocess.)
fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// Package-manager command that installs `dep`, if one is available.
pub fn install_command(dep: Dep) -> Option<(&'static str, Vec<&'static str>)> {
    if cfg!(windows) {
        return None;
    }
    let pkg = dep.binary();
    let has_brew = Path::new("/opt/homebrew/bin/brew").is_file() || on_path("brew");
    if has_brew {
        return Some(("brew", vec!["install", pkg]));
    }
    match dep {
        Dep::Mpv if on_path("apt") => Some(("sudo", vec!["apt", "install", "-y", pkg])),
        Dep::Ytdlp if on_path("pipx") => Some(("pipx", vec!["install", pkg])),
        Dep::Ytdlp if on_path("pip") => Some(("pip", vec!["install", pkg])),
        _ => None,
    }
}

/// Run the installer in the (suspended) terminal. The caller must have
/// restored the terminal and paused the input reader. Returns success.
pub fn run_install(dep: Dep) -> bool {
    let name = dep.binary();
    let success = match install_command(dep) {
        Some((program, args)) => {
            println!("\nInstalling {name}...\n  $ {program} {}", args.join(" "));
            match std::process::Command::new(program).args(&args).status() {
                Ok(status) if status.success() => {
                    println!("\n  {name} installed successfully.");
                    true
                }
                Ok(status) => {
                    println!(
                        "\n  Installation failed (exit code {}).",
                        status.code().map_or("?".to_string(), |c| c.to_string())
                    );
                    false
                }
                Err(e) => {
                    println!("\n  Failed to run {program}: {e}");
                    false
                }
            }
        }
        None => {
            println!("\n  Please install {name} manually from {}", dep.homepage());
            false
        }
    };
    println!("\nPress Enter to return to rustune...");
    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);
    success
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_line_extracts_name_and_version() {
        assert_eq!(
            version_line(b"mpv 0.38.0 Copyright \xC2\xA9 2000-2024\nbuilt on ..."),
            "mpv 0.38.0"
        );
        assert_eq!(version_line(b"\n2024.08.06\n"), "2024.08.06");
        assert_eq!(version_line(b""), "");
    }

    #[test]
    fn missing_program_is_not_on_path() {
        assert!(!on_path("rustune-definitely-not-a-real-binary"));
    }
}
