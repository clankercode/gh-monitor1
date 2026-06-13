//! `gh-monitor doctor` — diagnostic checks for the user's environment.
//!
//! Runs a fixed set of checks (config, PAT, GitHub API, GTK, tray,
//! display, filesystem) and prints one `[ OK   ] label: detail` line
//! per check, with optional ANSI color when stdout is a TTY. The
//! process then exits with code 0 (all OK), 1 (any FAIL), or 2
//! (any WARN and no FAIL).
//!
//! Network calls (username, orgs/repos) run with a 5-second timeout
//! inside a single short-lived tokio current-thread runtime so the
//! command works whether or not the rest of the app is running.

use std::borrow::Cow;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use gh_monitor_config::Config;

use crate::config_io::{config_path, default_config};

const DOCTOR_TIMEOUT: Duration = Duration::from_secs(5);
const GITHUB_API_BASE: &str = "https://api.github.com";

/// The outcome of a single doctor check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Check passed.
    Ok,
    /// Check passed with a caveat the user should know about. Exits 2.
    Warn,
    /// Check failed. Exits 1.
    Fail,
}

impl Status {
    /// The fixed label: `"OK"`, `"WARN"`, `"FAIL"`. Printers left-pad
    /// to four columns for alignment.
    pub fn label(self) -> &'static str {
        match self {
            Status::Ok => "OK",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
        }
    }

    pub fn is_fail(self) -> bool {
        matches!(self, Status::Fail)
    }

    pub fn is_warn(self) -> bool {
        matches!(self, Status::Warn)
    }

    /// SGR color parameter for this status: `32` (green), `33`
    /// (yellow), `31` (red).
    pub fn sgr(self) -> &'static str {
        match self {
            Status::Ok => "32",
            Status::Warn => "33",
            Status::Fail => "31",
        }
    }
}

/// One printable check result. `label` is `Cow` so static labels
/// (e.g. `"config"`, `"pat"`) don't allocate, while dynamic labels
/// (e.g. `"orgs/rust-lang"`) can be built at runtime.
#[derive(Debug, Clone)]
pub struct CheckResult {
    pub status: Status,
    pub label: Cow<'static, str>,
    pub detail: String,
}

impl CheckResult {
    pub fn ok(label: impl Into<Cow<'static, str>>, detail: impl Into<String>) -> Self {
        Self {
            status: Status::Ok,
            label: label.into(),
            detail: detail.into(),
        }
    }

    pub fn warn(label: impl Into<Cow<'static, str>>, detail: impl Into<String>) -> Self {
        Self {
            status: Status::Warn,
            label: label.into(),
            detail: detail.into(),
        }
    }

    pub fn fail(label: impl Into<Cow<'static, str>>, detail: impl Into<String>) -> Self {
        Self {
            status: Status::Fail,
            label: label.into(),
            detail: detail.into(),
        }
    }
}

/// Run all doctor checks, print results, and exit with the
/// appropriate code. Never returns to the caller on the success
/// path — `run` always terminates the process via `std::process::exit`
/// so the caller (`main.rs`) just propagates `Err` if the HTTP
/// client fails to build.
pub fn run() -> Result<()> {
    let path = config_path();
    let config = load_effective_config(&path);
    let http = build_http_client().context("building HTTP client")?;
    let use_color = io::stdout().is_terminal();

    let mut results: Vec<CheckResult> = Vec::with_capacity(8);
    results.push(check_config(&path));
    results.push(check_pat(&config));
    results.extend(run_network_checks(&config, &http));
    results.push(check_gtk());
    results.push(check_tray());
    results.push(check_display());
    results.push(check_filesystem(&path));

    let mut out = io::stdout().lock();
    for r in &results {
        writeln_check(&mut out, r, use_color).context("writing check result")?;
    }

    let code = exit_code(&results);
    std::process::exit(code);
}

/// Prefer the on-disk config if it exists and parses, otherwise fall
/// back to env-var defaults so the doctor can still inspect the
/// environment (e.g. when the file is missing or malformed).
fn load_effective_config(path: &Path) -> Config {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|body| toml::from_str::<Config>(&body).ok())
        .unwrap_or_else(default_config)
}

fn build_http_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(DOCTOR_TIMEOUT)
        .user_agent(format!("gh-monitor/{}", env!("CARGO_PKG_VERSION")))
        .build()
}

fn run_network_checks(config: &Config, http: &reqwest::Client) -> Vec<CheckResult> {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return vec![CheckResult::fail("network", format!("tokio: {e}"))],
    };
    rt.block_on(async {
        vec![
            check_username(config, http).await,
            check_orgs_repos(config, http).await,
        ]
    })
}

fn check_config(path: &Path) -> CheckResult {
    if !path.exists() {
        return CheckResult::warn(
            "config",
            format!(
                "file missing at {}; run `gh-monitor init` or set GH_MONITOR_* env vars",
                path.display()
            ),
        );
    }
    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(e) => return CheckResult::fail("config", format!("read failed: {e}")),
    };
    let cfg: Config = match toml::from_str(&body) {
        Ok(c) => c,
        Err(e) => return CheckResult::fail("config", format!("invalid TOML: {e}")),
    };
    if let Err(e) = cfg.validate() {
        return CheckResult::fail("config", format!("validation failed: {e}"));
    }
    CheckResult::ok("config", format!("valid, {} bytes", body.len()))
}

fn check_pat(config: &Config) -> CheckResult {
    if config.pat.trim().is_empty() {
        CheckResult::warn("pat", "no PAT set")
    } else {
        CheckResult::ok("pat", "set")
    }
}

async fn check_username(config: &Config, http: &reqwest::Client) -> CheckResult {
    let label: Cow<'static, str> = "username".into();
    let Some(user) = config.username.as_deref().filter(|s| !s.is_empty()) else {
        return CheckResult::warn(label, "not set");
    };
    let url = format!("{GITHUB_API_BASE}/users/{user}");
    match http.get(&url).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                CheckResult::ok(label, format!("@{user}: {status}"))
            } else {
                CheckResult::fail(label, format!("@{user}: {status}"))
            }
        }
        Err(e) => CheckResult::fail(label, format!("@{user}: network error: {e}")),
    }
}

async fn check_orgs_repos(config: &Config, http: &reqwest::Client) -> CheckResult {
    let (label, url) = if let Some(org) = config.orgs.first() {
        (
            Cow::Owned(format!("orgs/{org}")),
            format!("{GITHUB_API_BASE}/orgs/{org}/events"),
        )
    } else if let Some(repo) = config.repos.first() {
        let Some((owner, name)) = repo.split_once('/') else {
            return CheckResult::fail("repos", format!("malformed repo: {repo}"));
        };
        (
            Cow::Owned(format!("repos/{repo}")),
            format!("{GITHUB_API_BASE}/repos/{owner}/{name}/events"),
        )
    } else {
        return CheckResult::warn("orgs/repos", "none set");
    };
    let mut req = http
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if !config.pat.trim().is_empty() {
        req = req.header("Authorization", format!("Bearer {}", config.pat));
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                CheckResult::ok(label, format!("{status}"))
            } else {
                CheckResult::fail(label, format!("{status}"))
            }
        }
        Err(e) => CheckResult::fail(label, format!("network error: {e}")),
    }
}

#[cfg(target_os = "linux")]
fn check_gtk() -> CheckResult {
    match gtk::init() {
        Ok(()) => CheckResult::ok("gtk", "initialized"),
        Err(e) => CheckResult::fail("gtk", format!("gtk::init failed: {e}")),
    }
}

#[cfg(not(target_os = "linux"))]
fn check_gtk() -> CheckResult {
    CheckResult::ok("gtk", "n/a (non-Linux)")
}

fn check_tray() -> CheckResult {
    // Smoke test the data path of the tray-icon crate without
    // initializing GTK (covered by `check_gtk`) or attaching to the
    // system tray. If this builds, the tray crate is linked and the
    // icon codec works.
    const W: u32 = 4;
    const H: u32 = 4;
    let rgba = vec![0u8; (W * H * 4) as usize];
    match tray_icon::Icon::from_rgba(rgba, W, H) {
        Ok(_) => CheckResult::ok("tray", "icon builder ok"),
        Err(e) => CheckResult::fail("tray", format!("icon build failed: {e}")),
    }
}

fn check_display() -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        let has_display =
            std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
        if has_display {
            CheckResult::ok("display", "DISPLAY or WAYLAND_DISPLAY set")
        } else {
            CheckResult::warn("display", "neither DISPLAY nor WAYLAND_DISPLAY set")
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        CheckResult::ok("display", "ok")
    }
}

fn check_filesystem(path: &Path) -> CheckResult {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    if let Err(e) = std::fs::create_dir_all(dir) {
        return CheckResult::fail(
            "filesystem",
            format!("cannot create {}: {e}", dir.display()),
        );
    }
    // Write a tiny probe file, read it back, then delete it. This
    // exercises both read and write on the config dir.
    let probe = dir.join(".gh-monitor-doctor-probe");
    let body = b"ok".to_vec();
    if let Err(e) = std::fs::write(&probe, &body) {
        return CheckResult::fail("filesystem", format!("write {}: {e}", probe.display()));
    }
    let read_back = match std::fs::read(&probe) {
        Ok(b) => b,
        Err(e) => {
            let _ = std::fs::remove_file(&probe);
            return CheckResult::fail("filesystem", format!("read {}: {e}", probe.display()));
        }
    };
    let _ = std::fs::remove_file(&probe);
    if read_back != body {
        return CheckResult::fail("filesystem", "read/write mismatch");
    }
    CheckResult::ok("filesystem", format!("read+write ok: {}", dir.display()))
}

/// Format a check result as the single printable line. Pure; the
/// printing wrapper in `writeln_check` calls this for the
/// non-colored path.
pub fn format_check_line(r: &CheckResult) -> String {
    format!("[ {:<4} ] {}: {}", r.status.label(), r.label, r.detail)
}

/// Write one check result line to `w`, optionally ANSI-colored
/// (green/yellow/red on the status word; the rest is plain).
pub fn writeln_check<W: Write>(w: &mut W, r: &CheckResult, use_color: bool) -> io::Result<()> {
    if use_color {
        let padded = format!("{:<4}", r.status.label());
        writeln!(
            w,
            "[ \x1b[{}m{}\x1b[0m ] {}: {}",
            r.status.sgr(),
            padded,
            r.label,
            r.detail
        )
    } else {
        writeln!(w, "{}", format_check_line(r))
    }
}

/// Map a slice of check results to a process exit code:
/// 0 = all OK, 1 = any FAIL, 2 = any WARN (no FAIL).
pub fn exit_code(results: &[CheckResult]) -> i32 {
    if results.iter().any(|r| r.status.is_fail()) {
        1
    } else if results.iter().any(|r| r.status.is_warn()) {
        2
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_label_is_stable() {
        assert_eq!(Status::Ok.label(), "OK");
        assert_eq!(Status::Warn.label(), "WARN");
        assert_eq!(Status::Fail.label(), "FAIL");
    }

    #[test]
    fn status_predicates() {
        assert!(!Status::Ok.is_fail());
        assert!(!Status::Ok.is_warn());
        assert!(Status::Warn.is_warn());
        assert!(!Status::Warn.is_fail());
        assert!(Status::Fail.is_fail());
        assert!(!Status::Fail.is_warn());
    }

    #[test]
    fn status_sgr_codes_match_ansi() {
        assert_eq!(Status::Ok.sgr(), "32");
        assert_eq!(Status::Warn.sgr(), "33");
        assert_eq!(Status::Fail.sgr(), "31");
    }

    #[test]
    fn check_result_constructors() {
        let r = CheckResult::ok("config", "valid");
        assert_eq!(r.status, Status::Ok);
        assert_eq!(r.label, "config");
        assert_eq!(r.detail, "valid");

        let r = CheckResult::warn("pat", "no PAT set");
        assert_eq!(r.status, Status::Warn);
        assert_eq!(r.label, "pat");

        let r = CheckResult::fail("orgs/rust-lang", "401 Unauthorized");
        assert_eq!(r.status, Status::Fail);
        assert_eq!(r.label, "orgs/rust-lang");
        assert_eq!(r.detail, "401 Unauthorized");
    }

    #[test]
    fn check_result_accepts_dynamic_label() {
        let label: Cow<'static, str> = Cow::Owned(format!("repos/{}/{}", "o", "n"));
        let r = CheckResult::ok(label, "200 OK");
        assert_eq!(r.label, "repos/o/n");
    }

    #[test]
    fn format_check_line_pads_status_to_four_columns() {
        let r = CheckResult::ok("config", "valid, 87 bytes");
        let line = format_check_line(&r);
        // "[ OK   ]" — 4 chars between the brackets: "OK" + 2 padding
        // spaces, surrounded by the bracket's leading and trailing
        // spaces.
        assert!(
            line.starts_with("[ OK   ]"),
            "got {line:?} (expected leading `[ OK   ]`)"
        );
        assert!(line.ends_with("config: valid, 87 bytes"));
    }

    #[test]
    fn format_check_line_warn_and_fail() {
        let r = CheckResult::warn("pat", "no PAT set");
        let line = format_check_line(&r);
        assert!(line.starts_with("[ WARN ]"), "got {line:?}");
        assert!(line.ends_with("pat: no PAT set"));

        let r = CheckResult::fail("orgs/rust-lang", "401 Unauthorized");
        let line = format_check_line(&r);
        assert!(line.starts_with("[ FAIL ]"), "got {line:?}");
        assert!(line.ends_with("orgs/rust-lang: 401 Unauthorized"));
    }

    #[test]
    fn format_check_line_is_eight_chars_to_bracket_close() {
        // All three statuses should produce a line that is 8 chars
        // wide up to and including the closing bracket.
        for r in [
            CheckResult::ok("a", "b"),
            CheckResult::warn("a", "b"),
            CheckResult::fail("a", "b"),
        ] {
            let line = format_check_line(&r);
            let prefix = line
                .split(']')
                .next()
                .expect("line contains `]`")
                .to_string()
                + "]";
            assert_eq!(
                prefix.len(),
                8,
                "prefix {prefix:?} not 8 chars for {line:?}"
            );
        }
    }

    #[test]
    fn exit_code_no_results_is_zero() {
        assert_eq!(exit_code(&[]), 0);
    }

    #[test]
    fn exit_code_all_ok_is_zero() {
        let results = vec![CheckResult::ok("a", "x"), CheckResult::ok("b", "y")];
        assert_eq!(exit_code(&results), 0);
    }

    #[test]
    fn exit_code_warn_is_two() {
        let results = vec![CheckResult::ok("a", "x"), CheckResult::warn("b", "y")];
        assert_eq!(exit_code(&results), 2);
    }

    #[test]
    fn exit_code_fail_beats_warn() {
        let results = vec![
            CheckResult::ok("a", "x"),
            CheckResult::warn("b", "y"),
            CheckResult::fail("c", "z"),
        ];
        assert_eq!(exit_code(&results), 1);
    }

    #[test]
    fn exit_code_fail_only_is_one() {
        let results = vec![CheckResult::fail("a", "x")];
        assert_eq!(exit_code(&results), 1);
    }

    #[test]
    fn writeln_check_no_color_uses_plain_format_with_newline() {
        let r = CheckResult::ok("config", "valid");
        let mut buf = Vec::new();
        writeln_check(&mut buf, &r, false).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(s, "[ OK   ] config: valid\n");
    }

    #[test]
    fn writeln_check_with_color_wraps_status_in_ansi() {
        let r = CheckResult::fail("config", "bad");
        let mut buf = Vec::new();
        writeln_check(&mut buf, &r, true).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\x1b[31m"), "missing red SGR: {s:?}");
        assert!(s.contains("\x1b[0m"), "missing reset: {s:?}");
        assert!(s.contains("config: bad"));
        assert!(s.ends_with('\n'));
        // The visible text (ANSI stripped) must match the plain form.
        assert_eq!(strip_ansi(&s), "[ FAIL ] config: bad\n");
    }

    #[test]
    fn writeln_check_with_color_uses_green_for_ok() {
        let r = CheckResult::ok("pat", "set");
        let mut buf = Vec::new();
        writeln_check(&mut buf, &r, true).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\x1b[32m"), "missing green SGR: {s:?}");
    }

    #[test]
    fn writeln_check_with_color_uses_yellow_for_warn() {
        let r = CheckResult::warn("pat", "no PAT set");
        let mut buf = Vec::new();
        writeln_check(&mut buf, &r, true).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\x1b[33m"), "missing yellow SGR: {s:?}");
    }

    /// Remove the SGR (Select Graphic Rendition) ANSI escape codes
    /// from a string. Used by tests to compare the colored and
    /// non-colored outputs.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' && chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&c) = chars.peek() {
                    chars.next();
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}
