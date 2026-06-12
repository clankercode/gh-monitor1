# gh-monitor

A small, light, native (Linux, macOS, Windows) desktop app that shows a
**transparent, always-on-top overlay timeline** of GitHub activity for repos
and orgs you care about. Click anywhere else → clicks pass through. Hover the
pane → it becomes opaque. Click+drag to move. Click a node → open the
PR/issue/release in your browser.

Built with Rust + Iced. One binary per platform. No Electron, no Tauri.

![status](https://img.shields.io/badge/status-pre--alpha-orange)

## Features

- **Transparent overlay** — floats above your work, never in the way
- **Hover-to-capture** — clicks pass through when you're not interacting
- **Click+drag to move** — position persists across restarts
- **Animated timeline** — new events fade in, updated events pulse
- **Repo-grouped events** — PRs and issues from the same repo collapse into
  one node with counts and a humanized time range
- **Five event types** — PR opened, PR merged, issue opened, release
  published, new repo created
- **Single binary per platform** — no runtime, no installer needed

## Status

Pre-alpha. The scaffolding is in place. See `PLAN.md` for what we're building
and where we are in the plan.

## Quick start (dev)

Requires Rust 1.81+, `just`, and platform deps for Iced's wgpu backend
(see [Iced's prerequisites](https://github.com/iced-rs/iced#prerequisites)).

```bash
git clone https://github.com/xertrov/gh-monitor
cd gh-monitor
just build           # debug build
just test            # run all tests
just run             # run the app
```

## Quick start (release binary)

Pre-built binaries for Linux, macOS, and Windows are published on the
[Releases](https://github.com/xertrov/gh-monitor/releases) page. Pick your
platform, download, and run.

## Configuration

The app reads a `config.toml` from the platform's user config dir:

- Linux: `~/.config/gh-monitor/config.toml`
- macOS: `~/Library/Application Support/gh-monitor/config.toml`
- Windows: `%APPDATA%\gh-monitor\config.toml`

Schema:

```toml
# Personal access token (required)
pat = "ghp_..."

# Orgs to watch (events for these orgs will be fetched from /orgs/{org}/events)
orgs = ["rust-lang", "tokio-rs"]

# Individual repos to watch (in addition to the above orgs)
repos = ["octocat/Hello-World"]

# Optional: how often to poll, in seconds. Default 30.
poll_interval_secs = 30
```

## Architecture

See `PLAN.md` for the design pillars and `docs/architecture.md` for the
module/crate map. The TL;DR:

- `crates/gh` — GitHub REST client + polling loop. Pure logic, no GUI.
- `crates/timeline` — group events by repo, compress similar events,
  humanize time ranges. Pure logic, no GUI.
- `crates/config` — load/save TOML config.
- `crates/app` — Iced `Application` that ties the above together and owns
  the transparent always-on-top window, hover passthrough, click+drag,
  custom canvas rendering, and tray icon.

## Development

```bash
just                # list available tasks
just ci             # run fmt + lint + test + build-release (what CI runs)
just test-review    # review insta snapshots
just bloat          # see what's bloating the release binary
```

## License

TBD (likely MIT or Apache-2.0).
