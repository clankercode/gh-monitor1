//! Interactive first-time setup wizard (`gh-monitor init`).
//!
//! Walks the user through configuring their PAT, username, orgs, repos, and
//! poll interval, then writes the result to the platform config file.

#![allow(unsafe_code)]

use std::io::{self, BufRead, Write};

use anyhow::{Context, Result};
use gh_monitor_config::Config;

use crate::config_io::{config_path, save_config};

/// Run the interactive setup wizard. Reads from stdin, writes the resulting
/// config to the platform's user config dir.
pub fn run() -> Result<()> {
    let path = config_path();
    println!("Welcome to gh-monitor!\n");
    println!("This will set up your config file at: {}", path.display());
    println!();

    let pat = prompt_pat("Personal access token (required): ")?;
    let username = prompt_line("GitHub username (required): ")?;
    let orgs = parse_list(&prompt_line("Orgs to watch (comma-separated, or empty): ")?);
    let repos = parse_list(&prompt_line(
        "Repos to watch (comma-separated, or empty): ",
    )?);
    let poll_interval_secs = prompt_poll_interval("Poll interval in seconds [600]: ")?;

    let cfg = build_config(pat, username, orgs, repos, poll_interval_secs)
        .map_err(|e| anyhow::anyhow!("invalid configuration: {e}"))?;

    save_config(&cfg).context("saving config")?;

    println!();
    println!("\u{2713} Config written to {}", path.display());
    println!("\u{2713} Validated successfully");
    println!("Run `gh-monitor` to start the overlay.");

    Ok(())
}

/// Build a [`Config`] from raw user input and validate it. Returns the
/// validation error message on failure.
pub(crate) fn build_config(
    pat: String,
    username: String,
    orgs: Vec<String>,
    repos: Vec<String>,
    poll_interval_secs: u64,
) -> Result<Config, String> {
    let cfg = Config {
        pat,
        username: if username.trim().is_empty() {
            None
        } else {
            Some(username)
        },
        orgs,
        repos,
        poll_interval_secs,
        ..Config::default()
    };
    cfg.validate()?;
    Ok(cfg)
}

/// Parse a comma-separated list, trimming whitespace and dropping empties.
pub(crate) fn parse_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

fn prompt_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush().context("flushing stdout")?;
    let stdin = io::stdin();
    let mut handle = stdin.lock();
    let mut buf = String::new();
    handle.read_line(&mut buf).context("reading stdin")?;
    Ok(buf.trim().to_string())
}

fn prompt_pat(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush().context("flushing stdout")?;
    let stdin = io::stdin();
    let mut handle = stdin.lock();
    let mut buf = String::new();
    read_hidden_line(&mut handle, &mut buf).context("reading PAT")?;
    println!();
    Ok(buf.trim().to_string())
}

fn prompt_poll_interval(prompt: &str) -> Result<u64> {
    loop {
        let raw = prompt_line(prompt)?;
        if raw.is_empty() {
            return Ok(600);
        }
        match raw.parse::<u64>() {
            Ok(n) if n >= 5 => return Ok(n),
            Ok(_) => eprintln!("Poll interval must be 5 seconds or more."),
            Err(_) => eprintln!("Please enter a whole number of seconds."),
        }
    }
}

fn read_hidden_line<R: BufRead>(reader: &mut R, buf: &mut String) -> io::Result<()> {
    match install_echo_guard() {
        Some(guard) => {
            let result = reader.read_line(buf);
            drop(guard);
            let _ = result?;
            Ok(())
        }
        None => {
            let _ = reader.read_line(buf)?;
            Ok(())
        }
    }
}

/// Try to install a guard that disables terminal echo. Returns `Some` if echo
/// is now disabled (and the guard will restore it on drop), or `None` if
/// echo-disabling is not supported on this platform / handle. In the `None`
/// case, a warning has already been printed to stderr.
fn install_echo_guard() -> Option<EchoGuard> {
    EchoGuard::install()
}

#[cfg(unix)]
struct EchoGuard {
    fd: std::os::fd::RawFd,
    saved: Option<libc::termios>,
}

#[cfg(unix)]
impl EchoGuard {
    fn install() -> Option<Self> {
        use std::os::fd::AsRawFd;
        let fd = io::stdin().as_raw_fd();
        let is_tty = unsafe { libc::isatty(fd) != 0 };
        if !is_tty {
            eprintln!("warning: stdin is not a TTY; PAT may be visible");
            return None;
        }
        let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd, &mut termios) } != 0 {
            eprintln!("warning: could not read terminal attributes; PAT may be visible");
            return None;
        }
        let saved = termios;
        termios.c_lflag &= !libc::ECHO;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) } != 0 {
            eprintln!("warning: could not disable terminal echo; PAT may be visible");
            return None;
        }
        Some(Self {
            fd,
            saved: Some(saved),
        })
    }
}

#[cfg(unix)]
impl Drop for EchoGuard {
    fn drop(&mut self) {
        if let Some(saved) = self.saved.take() {
            unsafe {
                let _ = libc::tcsetattr(self.fd, libc::TCSANOW, &saved);
            }
        }
    }
}

#[cfg(not(unix))]
struct EchoGuard;

#[cfg(not(unix))]
impl EchoGuard {
    fn install() -> Option<Self> {
        eprintln!("warning: terminal does not support hiding input; PAT may be visible");
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_list_handles_commas_and_whitespace() {
        assert_eq!(parse_list(""), Vec::<String>::new());
        assert_eq!(parse_list("a"), vec!["a".to_string()]);
        assert_eq!(
            parse_list("a,b,c"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert_eq!(
            parse_list(" rust-lang , tokio-rs ,, "),
            vec!["rust-lang".to_string(), "tokio-rs".to_string()]
        );
    }

    #[test]
    fn build_config_roundtrip() {
        let pat = "ghp_test_token_abc".to_string();
        let username = "octocat".to_string();
        let orgs = vec!["rust-lang".to_string()];
        let repos = vec!["octocat/Hello-World".to_string()];
        let cfg = build_config(
            pat.clone(),
            username.clone(),
            orgs.clone(),
            repos.clone(),
            30,
        )
        .expect("valid config");
        assert_eq!(cfg.pat, pat);
        assert_eq!(cfg.username.as_deref(), Some(username.as_str()));
        assert_eq!(cfg.orgs, orgs);
        assert_eq!(cfg.repos, repos);
        assert_eq!(cfg.poll_interval_secs, 30);
        assert!(cfg.window_position.is_none());
        let toml_str = toml::to_string(&cfg).expect("serialize");
        let parsed: Config = toml::from_str(&toml_str).expect("parse");
        assert_eq!(cfg, parsed);
    }

    #[test]
    fn build_config_rejects_empty_pat() {
        let err =
            build_config(String::new(), "octocat".to_string(), vec![], vec![], 30).unwrap_err();
        assert!(err.contains("pat"), "unexpected error: {err}");
    }

    #[test]
    fn build_config_rejects_no_sources() {
        let err =
            build_config("ghp_abc".to_string(), String::new(), vec![], vec![], 30).unwrap_err();
        assert!(err.contains("username") || err.contains("orgs") || err.contains("repos"));
    }

    #[test]
    fn build_config_rejects_bad_repo_format() {
        let err = build_config(
            "ghp_abc".to_string(),
            "octocat".to_string(),
            vec![],
            vec!["nope".to_string()],
            30,
        )
        .unwrap_err();
        assert!(err.contains("owner/name"));
    }

    #[test]
    fn build_config_allows_empty_username_when_orgs_set() {
        let cfg = build_config(
            "ghp_abc".to_string(),
            String::new(),
            vec!["rust-lang".to_string()],
            vec![],
            30,
        )
        .expect("valid: orgs-only is OK");
        assert!(cfg.username.is_none());
        assert_eq!(cfg.orgs, vec!["rust-lang".to_string()]);
    }

    #[test]
    fn build_config_rejects_low_poll_interval() {
        let err = build_config(
            "ghp_abc".to_string(),
            "octocat".to_string(),
            vec![],
            vec![],
            2,
        )
        .unwrap_err();
        assert!(err.contains("5"));
    }
}
