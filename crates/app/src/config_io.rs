//! Config load/save helpers.

use std::path::PathBuf;

use anyhow::{Context, Result};
use gh_monitor_config::Config;

/// The platform-specific config file path. Does not check existence.
pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("gh-monitor")
        .join("config.toml")
}

/// A config file template shown to first-time users when they run
/// `gh-monitor config edit`. Safe to write to disk — it contains no
/// secrets and pulls nothing from the environment.
pub const CONFIG_TEMPLATE: &str = r#"# gh-monitor config
# See: https://github.com/clankercode/gh-monitor1

# Personal access token (required)
pat = ""

# GitHub username (used to fetch `received_events`)
# username = "octocat"

# Orgs to watch
# orgs = ["rust-lang"]

# Repos to watch ("owner/name")
# repos = ["octocat/Hello-World"]

# Poll interval in seconds
poll_interval_secs = 30
"#;

/// Write the config template to `path` if no file is there yet. Returns
/// `true` if a new file was written.
pub fn ensure_template(path: &std::path::Path) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, CONFIG_TEMPLATE).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

/// Load the config from disk, falling back to a default if the file
/// doesn't exist.
pub fn load_config() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        return Ok(default_config());
    }
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let cfg: Config =
        toml::from_str(&body).with_context(|| format!("parsing {}", path.display()))?;
    Ok(cfg)
}

/// Save the config to disk, creating parent dirs as needed.
pub fn save_config(cfg: &Config) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let body = toml::to_string_pretty(cfg).context("serializing config")?;
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// A fresh, empty config. Used when the file is missing. Environment
/// variables provide defaults for the PAT, username, and watched
/// orgs/repos so the app can run with no config file at all.
pub fn default_config() -> Config {
    let pat = std::env::var("GH_MONITOR_PAT").unwrap_or_default();
    let username = std::env::var("GH_MONITOR_USERNAME").ok();
    let orgs = std::env::var("GH_MONITOR_ORGS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    let repos = std::env::var("GH_MONITOR_REPOS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    Config {
        pat,
        username,
        orgs,
        repos,
        poll_interval_secs: 30,
        window_position: None,
    }
}
