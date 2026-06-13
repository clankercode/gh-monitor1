# Changelog

All notable changes to `gh-monitor` are documented here. The format is
based on [Keep a Changelog](https://keepachangelog.com/), and this
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.3.0] — 2026-06-13

### Added
- **`gh-monitor doctor`** — diagnostic command. Runs 8 checks (config,
  PAT, GitHub username/org/repo, GTK, tray, display, filesystem) and
  prints `[ OK ]` / `[ WARN ]` / `[ FAIL ]` lines with ANSI color when
  on a TTY. Exit code 0 (all OK), 1 (any FAIL), or 2 (any WARN).
- **Per-source polling status** — every GitHub source (`received`,
  each org, each repo) now reports its own status. The overlay's
  status banner shows e.g. `org/rust-lang: 401 Unauthorized` or
  `polling (1/3 ok)` when some sources are failing.

### Changed
- `PollItem` (in `gh_monitor_gh`) now carries a `&'static str` source
  label on all variants. `PollOutcome` is restructured into
  `Vec<PollSourceOutcome>` with `total_events()` / `total_errors()`
  helpers.
- `PollStatus` (in `gh_monitor_app`) is now a per-source
  `Vec<SourceStatus>` rather than a single global status.

## [0.2.0] — 2026-06-13

### Added
- **`gh-monitor init`** — interactive setup wizard. Walks through PAT,
  username, orgs, repos, and poll interval. Hides the PAT input on
  Unix (termios-based). Validates before writing; only writes on
  success. Non-TTY stdin falls back to a warning + plain read.
- **Test coverage in CI** — new `coverage` job in `ci.yml` (Linux,
  informational) using `taiki-e/install-action` and
  `cargo-llvm-cov`. `just coverage` / `just coverage-lcov` targets.
  README's Coverage section documents the baseline: 64.4% line
  coverage (config 92.8%, gh 90.0%, timeline 93.6%, app 20.9%).
- **`empty_state_lines(needs_setup, status) -> Vec<String>`** and
  **`status_banner_text(text) -> &str`** — pure `pub(crate)` helpers
  extracted from the canvas's draw functions. Three new unit tests
  in the app crate.

## [0.1.1] — 2026-06-12

### Added
- **Tray menu's "Show / Hide" item** — toggles the window's `Mode`
  between `Hidden` and `Windowed`. `state.hidden` tracks the current
  mode.
- **Empty-state UI** — when the timeline is empty, the canvas shows
  setup instructions if no PAT is configured, "No recent activity"
  otherwise. A red top banner surfaces `PollError` and `AuthError`.
- **Window position persistence** — subscribed to `window::events()`
  and filtered for `Event::Moved(Point)`. On move, the in-memory
  config is updated and `save_config` runs on a `Task::future` so
  the UI doesn't block on disk I/O. Position survives restarts.

### Fixed
- **Linux CI build** — added `libxdo-dev`, `libwayland-dev`,
  `libdbus-1-dev`, `libudev-dev`, `libxkbcommon-x11-0`,
  `libxcb1-dev`, `libx11-xcb-dev`, `libfontconfig1-dev` to the apt
  install block. The `libxdo-dev` was the actual culprit — the
  tray-icon crate's `tray-icon` link line failed without it.

## [0.1.0] — 2026-06-12

### Added
- **Initial release.** A transparent, always-on-top desktop overlay
  showing a GitHub activity timeline.
- **Iced 0.14 GUI** — transparent + always-on-top + decorationless
  window. Hover-to-capture mouse passthrough. Click+drag to move.
  Custom canvas rendering with grouped events and humanized time
  ranges.
- **Five event types** — PR opened, PR merged, issue opened, release
  published, new repo created. The last is rare and visually
  accented.
- **Animated timeline** — fade-in on insert, 0→1→0 pulse on
  update. Driven by `iced::animation::Animation<f32>` with
  `auto_reverse`.
- **Deep links** — click a node to open the PR/issue/release in
  the default browser.
- **System tray** with `Quit` menu item (Linux/macOS/Windows).
- **GitHub REST client + polling** — `received_events` for the
  user, `orgs/{org}/events` for each org, `repos/{o}/{r}/events`
  for each repo. Tokio background task. Surfaces auth and
  transient errors per source.
- **TOML config** — `~/.config/gh-monitor/config.toml` on Linux,
  `~/Library/Application Support/gh-monitor/config.toml` on macOS,
  `%APPDATA%\gh-monitor\config.toml` on Windows.
- **CLI subcommands** — `gh-monitor config {path, print, edit,
  validate}`, plus `--help` and `--version`.
- **Cross-platform CI** — `ci.yml` matrix on
  `ubuntu-latest` / `macos-latest` (x86_64 + aarch64) /
  `windows-latest` with `cargo fmt --check`, `cargo clippy
  --all-targets --all-features -- -D warnings`, `cargo test`,
  and release build.
- **Automated releases** — `release.yml` runs on every `v*` tag,
  builds platform artifacts, attaches them to a GitHub Release
  with sha256 checksums.
- **87 tests** across 4 crates — unit tests for pure logic,
  integration tests for the GitHub client (wiremock), snapshot
  tests for the timeline grouping, and proptests for the humanize
  function.

[Unreleased]: https://github.com/clankercode/gh-monitor1/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/clankercode/gh-monitor1/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/clankercode/gh-monitor1/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/clankercode/gh-monitor1/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/clankercode/gh-monitor1/releases/tag/v0.1.0
