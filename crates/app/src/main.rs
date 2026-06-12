use anyhow::Context;
use tracing::info;

use gh_monitor_app::config_io::load_config;
use gh_monitor_app::{run, AppSettings};

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

    run(settings).context("running app")?;
    Ok(())
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
