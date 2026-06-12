use std::process::Command;

use anyhow::{Context, Result};
use tracing::info;

use gh_monitor_app::config_io::{config_path, load_config, save_config};
use gh_monitor_app::tray;
use gh_monitor_app::{run, AppSettings};

fn main() -> Result<()> {
    init_tracing();

    // CLI subcommands. Anything unknown falls through to running the GUI.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        return handle_cli(&args);
    }

    let config = load_config().context("loading config")?;
    info!(
        username = ?config.username,
        orgs = ?config.orgs,
        repos = ?config.repos,
        poll_secs = config.poll_interval_secs,
        "loaded config"
    );

    // Start the system tray. The tray runs on its own event loop and
    // posts actions to the GUI via a tokio channel. If the tray
    // can't start (e.g. no GTK on Linux), we log and continue.
    let _tray = match tray::spawn() {
        Ok(h) => Some(h),
        Err(e) => {
            tracing::warn!(error = %e, "failed to start system tray; continuing without it");
            None
        }
    };

    let settings = AppSettings { initial: config };

    run(settings).context("running app")?;
    Ok(())
}

fn handle_cli(args: &[String]) -> Result<()> {
    match args[0].as_str() {
        "config" => handle_config(&args[1..]),
        "--version" | "-V" => {
            println!("gh-monitor {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "--help" | "-h" | "help" => print_help(),
        other => {
            eprintln!("unknown command: {other}");
            eprintln!("run `gh-monitor --help` for usage");
            std::process::exit(2);
        }
    }
}

fn handle_config(args: &[String]) -> Result<()> {
    if args.is_empty() {
        eprintln!("missing subcommand: config <path|print|edit|validate>");
        std::process::exit(2);
    }
    match args[0].as_str() {
        "path" => {
            println!("{}", config_path().display());
            Ok(())
        }
        "print" => {
            let cfg = load_config().context("loading config")?;
            println!("{}", toml::to_string_pretty(&cfg)?);
            Ok(())
        }
        "validate" => match load_config() {
            Ok(cfg) => match cfg.validate() {
                Ok(()) => {
                    println!("config is valid");
                    Ok(())
                }
                Err(e) => {
                    eprintln!("config is invalid: {e}");
                    std::process::exit(1);
                }
            },
            Err(e) => {
                eprintln!("failed to load config: {e:#}");
                std::process::exit(1);
            }
        },
        "edit" => {
            // Open $VISUAL or $EDITOR in the config file's parent
            // directory (creating the file if missing).
            let path = config_path();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            if !path.exists() {
                save_config(&default_config())?;
            }
            let editor = std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| {
                    if cfg!(target_os = "windows") {
                        "notepad".to_string()
                    } else {
                        "vi".to_string()
                    }
                });
            let status = Command::new(&editor)
                .arg(&path)
                .status()
                .with_context(|| format!("spawning editor `{editor}`"))?;
            std::process::exit(status.code().unwrap_or(1));
        }
        other => {
            eprintln!("unknown config subcommand: {other}");
            eprintln!("usage: gh-monitor config <path|print|edit|validate>");
            std::process::exit(2);
        }
    }
}

fn default_config() -> gh_monitor_config::Config {
    use std::env;
    gh_monitor_config::Config {
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

fn print_help() -> Result<()> {
    println!(
        "gh-monitor — transparent always-on-top GitHub activity overlay\n\
         \n\
         USAGE:\n  \
             gh-monitor [COMMAND]\n\
         \n\
         COMMANDS:\n  \
             config path           Print the config file path\n  \
             config print          Print the loaded config as TOML\n  \
             config edit           Open the config file in $EDITOR\n  \
             config validate       Validate the config and exit\n  \
             (no args)             Run the overlay app\n\
         \n\
         FLAGS:\n  \
             -h, --help            Print this help\n  \
             -V, --version         Print the version\n\
         \n\
         ENVIRONMENT:\n  \
             GH_MONITOR_PAT        Personal access token (if no config file)\n  \
             GH_MONITOR_USERNAME   GitHub username\n  \
             GH_MONITOR_ORGS       Comma-separated orgs to watch\n  \
             GH_MONITOR_REPOS      Comma-separated repos to watch (owner/name)\n  \
             RUST_LOG              tracing log filter (e.g. info,debug)\n"
    );
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
