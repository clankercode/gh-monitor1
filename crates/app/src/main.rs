use anyhow::Context;
use tracing::info;

use gh_monitor_app::{run, AppSettings};
use gh_monitor_config::Config;

fn main() -> anyhow::Result<()> {
    init_tracing();

    let config = load_config().context("loading config")?;
    info!(
        username = ?config.username,
        orgs = ?config.orgs,
        repos = ?config.repos,
        poll_secs = config.poll_interval_secs,
        "loaded config"
    );

    let settings = AppSettings { initial: config };

    run(settings).context("running app")
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .try_init();
}

fn load_config() -> anyhow::Result<Config> {
    let path = config_path();
    if !path.exists() {
        return Ok(default_config());
    }
    let body = std::fs::read_to_string(&path)?;
    Ok(toml::from_str(&body)?)
}

fn default_config() -> Config {
    use std::env;
    Config {
        pat: env::var("GH_MONITOR_PAT").unwrap_or_default(),
        username: env::var("GH_MONITOR_USERNAME").ok(),
        orgs: env::var("GH_MONITOR_ORGS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect(),
        repos: env::var("GH_MONITOR_REPOS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect(),
        poll_interval_secs: 30,
        window_position: None,
    }
}

fn config_path() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("gh-monitor")
        .join("config.toml")
}
