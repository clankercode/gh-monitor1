# gh-monitor

A small, light, native (Linux, macOS, Windows) desktop app that shows a
**transparent, always-on-top overlay timeline** of GitHub activity for repos
and orgs you care about. Click anywhere else → clicks pass through. Hover the
pane → it becomes opaque. Click+drag to move. Click a node → open the
PR/issue/release in your browser.

Built with Rust + Iced. One binary per platform. No Electron, no Tauri.

![status](https://img.shields.io/badge/status-pre--alpha-orange)
![build](https://github.com/clankercode/gh-monitor1/actions/workflows/ci.yml/badge.svg)

## Features

- **Transparent overlay** — floats above your work, never in the way
- **Hover-to-capture** — clicks pass through when you're not interacting
- **Click+drag to move** — position persists across restarts
- **Animated timeline** — new events fade in, updated events pulse
- **Repo-grouped events** — PRs and issues from the same repo collapse into
  one node with counts and a humanized time range
- **Five event types** — PR opened, PR merged, issue opened, release
  published, new repo created (the last is rare + stands out)
- **System tray** — Quit from the tray icon
- **Deep links** — click any event to open the PR/issue/release in your
  browser
- **Single binary per platform** — no runtime, no installer needed

## Screenshots

> *Coming soon — the app is in pre-alpha. Once you have a config and a PAT,
> the overlay looks like a small floating panel showing your activity
> timeline.*

## Quick start (dev)

Requires Rust 1.81+ and `just`. On Linux you also need GTK 3, libxdo, and
libappindicator for the tray icon. See [Iced's prerequisites](https://github.com/iced-rs/iced#prerequisites)
plus `libgtk-3-dev libxdo-dev libayatana-appindicator3-dev`.

```bash
git clone https://github.com/clankercode/gh-monitor1
cd gh-monitor1
just build
just test
just run
```

## Quick start (release binary)

Pre-built binaries for Linux, macOS, and Windows are published on the
[Releases](https://github.com/clankercode/gh-monitor1/releases) page. Pick
your platform, download, and run.

## Configuration

`gh-monitor` reads a `config.toml` from the platform's user config dir:

- Linux: `~/.config/gh-monitor/config.toml`
- macOS: `~/Library/Application Support/gh-monitor/config.toml`
- Windows: `%APPDATA%\gh-monitor\config.toml`

Use the CLI to manage it:

```bash
gh-monitor config path        # print the config file path
gh-monitor config print       # print the loaded config as TOML
gh-monitor config edit        # open the config file in $EDITOR
gh-monitor config validate    # validate the config and exit
```

### Config schema

```toml
# Personal access token (required)
pat = "ghp_..."

# GitHub username (used to fetch `received_events`)
username = "octocat"

# Orgs to watch (events fetched from /orgs/{org}/events)
orgs = ["rust-lang", "tokio-rs"]

# Individual repos to watch (in addition to the above orgs)
repos = ["octocat/Hello-World"]

# Poll interval in seconds. Default 30.
poll_interval_secs = 30
```

You can also point the app at a config by setting environment variables
before launch (used as defaults if the config file is missing):

```bash
GH_MONITOR_PAT=ghp_... GH_MONITOR_USERNAME=octocat gh-monitor
```

## Architecture

See [`docs/architecture.md`](docs/architecture.md) for the module/crate
responsibilities and data flow. The TL;DR:

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
just coverage       # generate an HTML coverage report (needs cargo-llvm-cov)
just coverage-lcov  # generate lcov.info (used by CI)
just bloat          # see what's bloating the release binary
```

### Coverage

Code coverage is produced with [`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov)
in the `coverage` CI job (Linux only — LLVM coverage is Linux-friendly). The
job writes `lcov.info`, uploads it as a build artifact, and optionally pushes
it to Coveralls when the `COVERALLS_REPO_TOKEN` secret is set. Coverage is
**informational only** — it is not a CI gate.

Install `cargo-llvm-cov` locally with:

```bash
cargo install cargo-llvm-cov --locked
rustup component add llvm-tools-preview
```

#### Current baseline

The pure-logic crates carry the test load — `gh-monitor-gh` and
`gh-monitor-timeline` are at ~90-100% line coverage — while the Iced GUI
crate (`gh-monitor-app`) is intentionally light on tests at this stage
(it needs an offscreen render target to test the canvas). Workspace-wide
baseline (line coverage, `just coverage-lcov` on a clean tree):

| Crate                | Lines | Coverage |
| -------------------- | -----:| --------:|
| `gh-monitor-config`  |    69 |    92.8% |
| `gh-monitor-gh`      |   801 |    90.0% |
| `gh-monitor-timeline`|   718 |    93.6% |
| `gh-monitor-app`     |   998 |    20.9% |
| **TOTAL**            | **2586** | **64.4%** |

The project follows the conventions in [`AGENTS.md`](AGENTS.md).

## Status

Pre-alpha. Core functionality is implemented (transparent overlay, hover
passthrough, click+drag, deep links, animations, tray icon, polling).
What's still in progress:

- Window-position persistence on move (currently restored on boot only)
- GUI settings panel (use the CLI for now: `gh-monitor config edit`)
- Headless Iced smoke test (deferred — needs an offscreen render target)

See [`PLAN.md`](PLAN.md) for the full plan and open questions.

## License

MIT or Apache-2.0, at your option.
