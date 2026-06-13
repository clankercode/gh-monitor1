//! Config load/save helpers.

use std::path::{Path, PathBuf};

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

/// Save the config to disk, creating parent dirs as needed. The write
/// is atomic: we serialize to a `path.tmp` sibling and `rename` it over
/// the target. On POSIX `rename` is atomic; on Windows
/// `std::fs::rename` is implemented via `MoveFileEx(REPLACE_EXISTING)`,
/// which atomically replaces the target. A kill mid-write leaves the
/// old file untouched.
pub fn save_config(cfg: &Config) -> Result<()> {
    let path = config_path();
    save_config_to(&path, cfg)
}

/// Atomic write of `cfg` to `path`. Splits the work so tests can drive
/// the write into a temp dir without depending on the platform config
/// location.
pub(crate) fn save_config_to(path: &Path, cfg: &Config) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    let body = toml::to_string_pretty(cfg).context("serializing config")?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, &body).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use gh_monitor_config::schema::WindowPosition;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// Build a unique temp path. We use `std::env::temp_dir()` plus an
    /// atomic counter and the process id so parallel tests don't
    /// collide. The directory is not cleaned up automatically — the
    /// test removes the leaf files it creates.
    fn temp_path(name: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!(
                "gh-monitor-config-io-test-{}-{}-{}",
                std::process::id(),
                n,
                name
            ))
            .join("config.toml")
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("toml.tmp"));
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }

    fn sample_cfg() -> Config {
        Config {
            pat: "ghp_test".to_string(),
            username: Some("octocat".to_string()),
            orgs: vec!["rust-lang".to_string()],
            repos: vec!["octocat/Hello-World".to_string()],
            poll_interval_secs: 30,
            window_position: Some(WindowPosition { x: 100, y: 200 }),
        }
    }

    #[test]
    fn save_config_to_writes_final_file_atomically() {
        let path = temp_path("atomic");
        let cfg = sample_cfg();
        save_config_to(&path, &cfg).expect("save should succeed");
        // Final file is on disk.
        let body = std::fs::read_to_string(&path).expect("final file should exist");
        assert!(body.contains("pat = \"ghp_test\""));
        assert!(body.contains("rust-lang"));
        // No stale `.tmp` is left behind.
        let tmp = path.with_extension("toml.tmp");
        assert!(
            !tmp.exists(),
            "stale tmp file left behind at {}",
            tmp.display()
        );
        cleanup(&path);
    }

    #[test]
    fn save_config_to_overwrites_existing_file() {
        let path = temp_path("overwrite");
        let mut cfg = sample_cfg();
        save_config_to(&path, &cfg).expect("first save");
        cfg.pat = "ghp_new_token".to_string();
        save_config_to(&path, &cfg).expect("second save");
        let body = std::fs::read_to_string(&path).expect("final file should exist");
        assert!(body.contains("ghp_new_token"));
        let tmp = path.with_extension("toml.tmp");
        assert!(!tmp.exists(), "stale tmp after overwrite");
        cleanup(&path);
    }

    #[test]
    fn save_config_to_creates_parent_dirs() {
        // The path is two levels deep; `save_config_to` should mkdir -p.
        let path = temp_path("nested/deep");
        let cfg = sample_cfg();
        save_config_to(&path, &cfg).expect("save with nested parent");
        assert!(path.exists());
        cleanup(&path);
    }
}
