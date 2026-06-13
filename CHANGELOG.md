# Changelog

All notable changes to `gh-monitor` are documented here. The format is
based on [Keep a Changelog](https://keepachangelog.com/), and this
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- **Demo mode.** A "🎬 Demo" button in the top-right of the canvas
  replays a 120-second scripted sequence of ten fake GitHub events
  across four repos (`rust-lang/rust`, `tokio-rs/tokio`,
  `acme-corp/secret-project`, `serde-rs/serde`). The sequence
  exercises every animation path the real poll path produces: a
  new-node fade-in (rust-lang/rust, tokio-rs/tokio,
  serde-rs/serde), an update pulse when a count goes from 1 → 2 →
  3 in the same `(repo, kind)` group, a new group when a different
  kind is added (rust-lang/rust gets `PrOpened`, then `PrMerged`,
  then `IssueOpened`; tokio-rs/tokio gets `PrOpened`, then
  `IssueOpened`, then a `PrOpened` pulse), a "new repo created"
  standalone (acme-corp/secret-project with the gold accent), and
  a release event. Events fire 1.0 s apart on whole-second
  boundaries. While the demo is active the canvas shows a "Demo
  running — XXs left" pill that counts down to 0; when the window
  elapses the demo state is cleared and the final timeline stays
  visible. Clicking the button again re-runs the script from a
  clean slate. The demo is pure — it does not call the GitHub
  API, write to the config file, or persist across restarts.
  Implementation lives in `crates/app/src/demo.rs` (the
  `DemoState` schedule + `drain_due`); the canvas renders the
  button and indicator and hit-tests the button in
  `crates/app/src/canvas.rs`; the `iced::time::every(100ms)`
  subscription drives `Message::FrameTick` in
  `crates/app/src/app.rs`. New tests cover the script shape, the
  scheduled offsets, the `drain_due` cursor advance, the
  `FrameTick` → `apply_events` integration, the auto-clear at
  `DEMO_TOTAL_SECS`, and the button/indicator geometry.

## [1.0.1] — 2026-06-13

Two targeted polish fixes on top of v1.0.0. No behaviour change for
the user-visible overlay; the two fixes tighten the internal
contracts.

### Fixed
- **Type-safe `ClientError` over the poll channel.** The poller
  previously emitted per-source errors as opaque `String`s, and the
  GUI classified them with substring matches like
  `e.contains("401") || e.contains("403") || e.contains("unauthorized")`
  in `crates/app/src/app.rs`. This broke silently if the underlying
  `ClientError` `Display` impl ever changed wording. v1.0.1 plumbs the
  typed `gh_monitor_gh::ClientError` through `PollItem::Cycle::errors`
  and the `Message::PolledCycle` envelope, and the GUI now matches on
  the variants: `ClientError::Unauthorized` and
  `ClientError::RateLimited { .. }` are the explicit "auth error"
  signal; everything else (`Server`, `Http`, `Parse`, `Events`) is
  "transient". A new test
  (`polled_cycle_distinguishes_auth_from_transient_via_typed_errors`)
  feeds both classes through the same cycle and asserts the
  per-source `PollStatus` records each in the right `SourceStatusKind`
  — no string matching on the hot path. `ClientError` is now
  `#[derive(Clone)]` (the `Http` variant stores the `reqwest::Error`
  `Display` as a `String` so it can flow through Iced messages and
  the poller's `mpsc` channel) and a manual
  `From<reqwest::Error> for ClientError` impl keeps the `?` operator
  ergonomic in `get_events`.
- **URL scheme check in `open_url`.** `crates/app/src/link.rs::open_url`
  used to call `open::that(url)` for any string the canvas passed in.
  The canvas only ever passes URLs from `node.target_url` (GitHub API
  output or our hard-coded `https://github.com/{repo}`), so this was
  safe in practice — but it was one bug away from spawning a browser
  pointed at `javascript:alert(1)`. v1.0.1 parses the URL with
  `url::Url` and only proceeds for `http`/`https`; everything else
  (non-HTTP schemes, malformed strings) is logged at WARN and dropped.
  A new `open_url_with` helper takes the opener as a closure so tests
  can stub the browser launch; six tests cover `javascript:`,
  `file:`, `data:`, malformed strings, `http://`, and `https://`
  inputs.

## [1.0.0] — 2026-06-13

The first stable release. All v1 features from `PLAN.md` are
implemented and tested. The project is production-ready.

### Changed
- Workspace `version` bumped to `1.0.0`. `gh-monitor --version` and
  the User-Agent header now report `1.0.0` (was `0.1.0` since the
  first tag).
- `Cargo.toml` `repository` field now points at
  `clankercode/gh-monitor1` (the actual GitHub remote).
- `docs/architecture.md` rewritten to match the v0.3.0+ data flow
  (`Message::PolledCycle`, `PollItem::Cycle`, debounced position
  saves, single-instance lock).
- README's MSRV is now `1.89` (was `1.81`, bumped in v0.3.2 for
  `std::fs::File::try_lock`). Pre-alpha badge removed.

## [0.3.2] — 2026-06-13

### Fixed
- **Stricter repo validation.** `Config::validate` previously accepted
  any string containing a `/` as a `repos` entry, so `"/x"`, `"x/"`,
  and `"a/b/c"` all passed. The poller then silently dropped the
  malformed entry in `poll_once` and the source-label index
  desynchronised — the next valid repo picked up the previous repo's
  label. `Config::validate` now requires `owner/name` form (split on
  the first `/`, both halves non-empty, name has no further `/`); the
  poller's `intern_sources` also filters malformed repos at the
  source so a hand-edited config can't desync the labels. Validation
  errors are now surfaced on the overlay's status banner via the
  `Message::PolledCycle` path, not just logged.
- **Single-instance enforcement.** Two `gh-monitor` processes would
  both poll GitHub (doubling the rate-limit pressure) and fight for
  the tray icon. `main` now takes an exclusive `flock`-style lock
  on `<config_dir>/gh-monitor.lock` before starting the GUI; a
  second instance exits with a clear "another instance of gh-monitor
  is already running; lock: <path>" message. CLI subcommands
  (`init`, `doctor`, `config`, `--version`) are unaffected and can
  run alongside the GUI. The lock is released automatically when the
  process exits (the underlying file handle is closed by `Drop` /
  the OS).
- **Rate-limit (429) reset handling.** `ClientError::RateLimited`
  is now a struct variant carrying the `X-RateLimit-Reset` Unix
  timestamp (or a `Retry-After`-derived fallback) so the poller can
  format a user-friendly "rate-limited until 2024-01-15 14:30:00 UTC"
  message for the status banner. Previously a 429 produced a flat
  "rate-limited by GitHub" string with no reset hint. The poller
  still backs off 5 s on any error; a follow-up could sleep until
  the reset time.
- **Silently-swallowed poller-construction errors.**
  `install_poller_if_configured` now records config-validation
  failures (and `Poller::new` failures from the poller subscription)
  in a `static` and emits a `Message::PolledCycle` with a
  `("poller", err)` source so the existing status banner picks them
  up. Previously both paths just `warn!`-logged and returned, and
  the user saw "nothing happens".

### Changed
- `ClientError::RateLimited` is now `RateLimited { reset_at: Option<u64> }`.
  A new `rate_limit_banner` helper in `gh-monitor-gh::polling`
  produces the user-facing string. The poller run loop special-cases
  the variant to call this helper; all other errors still go through
  the `Display` impl.
- The poller subscription now checks `POLL_CONSTRUCTION_ERROR`
  before draining `POLL_BUILD`. On a non-empty static, it emits one
  `PolledCycle` with the recorded error and exits; on a missing
  static, it proceeds as before.
- `Cargo.toml`: workspace `rust-version` bumped from 1.81 to 1.89 to
  use stable `std::fs::File::try_lock` for the single-instance
  lockfile.

### Tests
- 13 new unit tests:
  - `repo_leading_slash_fails`, `repo_trailing_slash_fails`,
    `repo_too_many_slashes_fails`, `repo_minimal_form_ok`,
    `repo_realistic_form_ok` (config schema).
  - `intern_sources_skips_malformed_repos`,
    `intern_sources_keeps_all_malformed_when_none_valid`,
    `rate_limit_banner_with_reset_at_is_user_friendly`,
    `rate_limit_banner_without_reset_at_is_generic`,
    `rate_limit_429_with_reset_header_surfaces_user_message`
    (gh crate).
  - `rate_limit_with_x_ratelimit_reset_header`,
    `rate_limit_with_retry_after_falls_back`,
    `parse_rate_limit_reset_prefers_x_header`,
    `parse_rate_limit_reset_handles_garbage` (gh crate).
  - `second_acquire_fails_while_first_holds`,
    `second_acquire_succeeds_after_first_dropped`, `path_is_recorded`
    (single-instance module).
  - `install_poller_records_validation_error_for_bad_repo`,
    `install_poller_records_validation_error_for_leading_slash_repo`,
    `install_poller_succeeds_for_valid_config`,
    `polled_cycle_surfaces_construction_error` (app).
  Workspace total is now 144 tests.

## [0.3.1] — 2026-06-13

### Fixed
- **Per-cycle poll application.** The v0.3.0 poller emitted one
  message per source and `apply_events` rebuilt the snapshot from
  each batch, so the last source polled in a cycle "won" and the
  previous sources' nodes were animated out — the timeline flickered
  to a single source every cycle. The poller now emits a single
  `PollItem::Cycle` per tick carrying every source's batch plus its
  errors, and the app applies the flattened events in one shot.
- **Debounced window-position saves.** `Message::WindowMoved` fired
  ~60×/sec during a drag; the old handler wrote the config file on
  every event. A drag did hundreds of overlapping disk writes. Now
  writes are throttled to one per 500 ms; the in-memory config is
  always up to date and the next eligible save picks up the latest
  position. A `config_save_pending` flag is flushed synchronously
  on `Message::Escape` and `TrayAction::Quit` so the user's last
  move is never lost.
- **Atomic config writes.** `save_config` previously truncated and
  wrote the config file in place, so a kill mid-write would corrupt
  the file and `load_config` would fail on next start. The helper
  now writes to `<path>.toml.tmp` and renames over the target;
  `rename` is atomic on POSIX and `MoveFileEx(REPLACE_EXISTING)` on
  Windows, so a kill mid-write leaves the previous good file
  intact.

### Changed
- `PollItem` is now a single-variant enum:
  `Cycle { events: Vec<(&'static str, Vec<RawEvent>)>, errors: Vec<(&'static str, String)> }`.
  The `Events` / `Error` / `AuthError` variants are gone — auth and
  transient errors are both reported in `errors` (the message string
  tells them apart) and the GUI formats them the same way in the
  status banner.
- `Message` in `gh_monitor_app` is now `PolledCycle { events, errors }`
  instead of three variants (`Polled` / `PollError` / `AuthError`).
- `Config` save is now done through a private `save_config_to(path,
  cfg)` helper. `save_config` is a thin wrapper that uses
  `config_path()`. The helper is `pub(crate)` so tests can drive
  the write into a temp dir.

### Tests
- 10 new unit tests (`save_config_to_writes_final_file_atomically`,
  `save_config_to_overwrites_existing_file`,
  `save_config_to_creates_parent_dirs`, three
  `should_save_position_*` cases,
  `polled_cycle_applies_all_sources_in_one_shot`,
  `polled_cycle_records_per_source_errors`,
  `window_moved_first_event_records_pending_false`,
  `window_moved_within_debounce_window_marks_pending`) and a
  renamed/rewritten `run_emits_one_cycle_per_tick`. Workspace total
  is now 123 tests.

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

[Unreleased]: https://github.com/clankercode/gh-monitor1/compare/v0.3.2...HEAD
[0.3.2]: https://github.com/clankercode/gh-monitor1/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/clankercode/gh-monitor1/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/clankercode/gh-monitor1/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/clankercode/gh-monitor1/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/clankercode/gh-monitor1/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/clankercode/gh-monitor1/releases/tag/v0.1.0
