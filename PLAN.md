# gh-monitor — Plan

A small, light, native (Linux, macOS, Windows) desktop app that displays a
**transparent, always-on-top overlay timeline** of GitHub activity for
user-selected repos and orgs.

## Vision

A floating pane on your desktop showing what just happened across the GitHub
repos and orgs you care about. Click anywhere else → clicks pass through to the
app underneath. Hover the pane → it becomes opaque and clickable. Click+drag
anywhere on it → move it. Click a node → open the PR/issue/release in your
browser. The pane is **ambient** — it lives in your peripheral vision, gets out
of the way, and gently pulses when something new arrives.

## Why this exists (market gap)

Existing GitHub activity tools fall into three buckets, none of which are this:

- **Menubar inboxes** (Gitify, Neat, Octobox) — click an icon, get a popup panel.
  Not ambient. Cluttered.
- **Web apps** (Octobox, GitNews) — out of band, requires context switch.
- **Dead multi-column dashboards** (DevHub, ~2020) — TweetDeck-for-GitHub. None
  used the desktop surface.

**The overlay is the product.** No existing tool floats activity on top of your
real work, ambient and glanceable.

## Design pillars

1. **Ambient by default.** Transparent at rest. Clicks pass through. Opaque
   only on hover.
2. **Tasteful motion.** New events fade in. Updated events pulse subtly. No
   bouncing, no flashing.
3. **Glanceable, not scannable.** Group similar events. Show counts. Show
   humanized time ranges ("1–3 hrs ago"). The pane should be readable in 2
   seconds.
4. **Click-to-act.** Every event is a deep link to the source.
5. **One binary per platform.** No Electron. No Tauri. No runtime.

## Feature scope (v1)

### In scope
- Transparent always-on-top overlay window (decorationless, frameless)
- Mouse-passthrough that toggles on hover-enter / hover-leave
- Click+drag the pane to reposition (position persists)
- Polls GitHub Events API every 10 min (configurable) for `received_events` and per-org
  `orgs/{org}/events`
- Timeline rendering with grouping by repo, compression of similar events,
  humanized time ranges
- Five event types: PR opened, PR merged, issue opened, release published,
  new repo created
- "New repo created" gets a distinct visual treatment (rare + important)
- PAT-based auth, no OAuth flow for v1
- Animated additions (opacity fade-in) and updates (subtle pulse)
- Tray/menubar icon to quit and re-open settings
- Persist position, repo list, and PAT to a config file
- Three platform builds (Linux, macOS, Windows) via GitHub Actions

### Out of scope (v1)
- OAuth flow (PAT only)
- Webhooks (polling only)
- Linear/Jira/other forges
- Multiple accounts
- Filtering / rules / muting (just show all)
- Notifications / sound
- A mobile client
- A web client

## Tech stack (IGC-selected)

| Layer | Choice | Why |
|-------|--------|-----|
| Language | **Rust** | Single language, native compilation, small binaries, mature GUI ecosystem |
| GUI framework | **Iced 0.14** | Elm-architecture fits event streams; first-class transparency, mouse passthrough, always-on-top, animations, canvas, drag-to-move; markdown widget built-in |
| Async runtime | **tokio** | De-facto standard; integrates with `reqwest` and `Subscription` |
| HTTP | **reqwest** | Standard; supports rustls for a no-OpenSSL build |
| Serialization | **serde** + **serde_json** | Standard |
| Config | **toml** + **serde** | Human-editable; standard |
| Tray icon | **tray-icon** (cross-platform crate) | System-tray support for Iced is limited; we use a small standalone tray |
| GitHub API | REST `events` endpoints | Simpler than GraphQL for v1; `received_events`, `orgs/{org}/events`, `repos/{o}/{r}/events` |
| Time | **chrono** | Standard; humanize crate for "1–3 hrs ago" |
| Logging | **tracing** | Structured; integrates with `tracing-subscriber` |
| Testing | **insta** (snapshot), **wiremock** (HTTP) | Snapshot for grouping logic; wiremock for API client |
| CI/CD | **GitHub Actions** matrix | Build & test on Linux/Mac/Windows; release on tag |

**Fallbacks** (in priority order, if Iced blocks us):
1. egui 0.34 + `egui_overlay` pattern (proven overlay ref impl)
2. Slint with custom platform FFI for passthrough
3. Qt 6 (C++) — last resort due to size

## Architecture

```
gh-monitor/
├── Cargo.toml                # workspace root
├── justfile                  # task runner
├── README.md
├── AGENTS.md                 # repo norms for AI agents
├── .gitignore
├── .dockerignore
├── .github/
│   └── workflows/
│       ├── ci.yml            # build + test on 3 OS
│       └── release.yml       # build release artifacts on tag
├── crates/
│   ├── gh/                   # GitHub API client (pure logic, no Iced)
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── auth.rs       # PAT handling
│   │   │   ├── client.rs     # reqwest wrapper
│   │   │   ├── events.rs     # event types + parsing
│   │   │   └── polling.rs    # poll loop with backoff
│   │   ├── tests/
│   │   │   ├── events_parsing.rs
│   │   │   └── wiremock_fixtures/
│   │   └── Cargo.toml
│   │
│   ├── timeline/             # grouping/compression model (pure logic)
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── group.rs      # group events by repo
│   │   │   ├── compress.rs   # compress similar events
│   │   │   ├── humanize.rs   # "1–3 hrs ago"
│   │   │   └── snapshot.rs   # point-in-time state used for animations
│   │   ├── tests/
│   │   │   ├── group_snapshots.rs
│   │   │   └── humanize.rs
│   │   └── Cargo.toml
│   │
│   ├── config/               # persistence (position, repos, PAT)
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   └── schema.rs
│   │   └── Cargo.toml
│   │
│   └── app/                  # binary — Iced GUI, owns everything
│       ├── src/
│       │   ├── main.rs
│       │   ├── app.rs        # Iced Application
│       │   ├── overlay.rs    # passthrough + hover state machine
│       │   ├── canvas.rs     # custom timeline canvas
│       │   ├── animation.rs  # per-event fade/pulse state
│       │   ├── link.rs       # open URLs in default browser
│       │   └── tray.rs       # tray-icon integration
│       ├── tests/
│       └── Cargo.toml
└── docs/
    └── architecture.md
```

### Data flow

```
tokio runtime
  └─ gh::polling::Subscription ─► Vec<RawEvent>
       │                              │
       │                              ▼
       │                       timeline::group
       │                              │
       │                              ▼
       │                       timeline::compress
       │                              │
       │                              ▼
       │                       timeline::snapshot
       │                              │
       ▼                              ▼
   app::Message::Tick ──────► app::State (current snapshot + animation state)
                                      │
                                      ▼
                                app::view() ──► Iced canvas
                                      │
                                      ▼
                                Window (transparent, AOT, click-through)
```

### Key design decisions

- **`crates/gh` is pure logic + tokio** — no Iced dependency. Tested with
  `wiremock`. Can be reused or extracted later.
- **`crates/timeline` is pure logic** — no async, no GUI. Snapshot-tested with
  `insta`. This is the heart of the product; test it heavily.
- **`crates/app` is the binary** — owns the Iced `Application`, `Subscription`,
  and rendering. It glues `gh` + `timeline` together.
- **Animations are state, not effects** — each timeline node holds an
  `Animation<f32>` (opacity) and a `pulse: Animation<f32>`. The view reads
  `interpolate_with(|s| s, now)` every frame. No rolling our own ticker.

### Application state (Iced Elm)

```rust
struct State {
    /// Grouped, compressed timeline nodes.
    nodes: Vec<TimelineNode>,
    /// What was rendered last frame — used to diff and detect adds/updates.
    prev: Vec<TimelineNode>,
    /// Window position (persisted).
    position: (f32, f32),
    /// Hover state (drives passthrough).
    hovered: bool,
    /// Auth status.
    auth: AuthState,
    /// Polling status.
    poll: PollStatus,
    /// Per-node animation state, keyed by stable id.
    animations: HashMap<NodeId, NodeAnimation>,
}

enum Message {
    Tick(Instant),
    ReposUpdated(Vec<TimelineNode>),
    HoverEntered,
    HoverLeft,
    DragWindow,
    OpenUrl(url::Url),
    PositionChanged((f32, f32)),
    TrayAction(TrayAction),
}

struct NodeAnimation {
    /// Opacity 0..1. Tweened from 0 -> 1 on insert, then stays.
    opacity: Animation<f32>,
    /// Pulse 0..1, transient. 0 -> 1 -> 0 over 600ms on update.
    pulse: Animation<f32>,
    /// When this node first appeared.
    inserted_at: Instant,
    /// When this node was last updated.
    updated_at: Instant,
}
```

### Event grouping & compression

The timeline is a list of **nodes**. Each node represents one of:

- **A repo group** with `(event_type, count)` pairs and a humanized time range.
  Example: `acme/api · 3 PRs opened, 1 merged · 1–3 hrs ago`.
- **A standalone event** that is rare and important (new repo created). Stands
  out visually.

Compression rules:
- Events of the same type in the same repo within the last N hours collapse
  into one node with a count. N starts at 3 (configurable).
- The node's time range is the span from the earliest to the latest event in
  the group, humanized.
- New repo creation is **never** compressed — always a standalone node.

Visual treatment:
- New nodes fade in over 400ms with `Animation<f32>`.
- Updated nodes (count went from 3 to 4) get a 600ms pulse: a subtle glow that
  grows and fades.
- "New repo" nodes get a distinct accent color and a star icon.

### Mouse passthrough state machine

```
        HoverEntered              HoverLeft
Idle ────────────────► Active ────────────────► Idle
  │                     │                       │
  │                     │                       │
  └─ passthrough(true)  │  passthrough(false) ◄─┘
                        │
                        └─ render at full opacity
```

- `Active` state: passthrough disabled, opacity 1.0, mouse events captured.
- `Idle` state: passthrough enabled, opacity 0.3, mouse events pass through to
  the app behind.

Note: on Wayland, full passthrough is the only model winit exposes (no
per-region hit-test on regular toplevels). The state machine still works: on
hover-enter, `disable_mouse_passthrough`; on hover-leave, `enable_mouse_passthrough`.

## Implementation phases

### Phase 0 — Scaffolding (0.5 day)
- Cargo workspace + 4 crates stubbed
- `justfile` with `build`, `test`, `lint`, `run`, `release`
- `AGENTS.md` with the conventions
- `.gitignore`, `.dockerignore`
- Base `README.md`
- One passing `cargo test` per crate

### Phase 1 — GitHub client (1 day)
- `crates/gh` — REST client, PAT, event types, polling loop
- Wiremock-based tests for parsing + poll loop
- 1 GH integration test (gated, reads `GH_TOKEN` env)

### Phase 2 — Timeline model (1 day)
- `crates/timeline` — group, compress, humanize, snapshot diff
- `insta` snapshot tests
- Pure functions, no async, no GUI

### Phase 3 — Config (0.25 day)
- `crates/config` — TOML schema for `repos`, `orgs`, `pat`, `position`
- Load/save to platform-specific config dir
- Round-trip tests

### Phase 4 — Iced app: window + passthrough (1 day)
- `crates/app` — Iced `Application`, transparent window, AlwaysOnTop, no
  decorations
- Hover state machine, passthrough toggle
- "Hello timeline" placeholder rendered in the canvas
- `just run` works on the dev box

### Phase 5 — Timeline canvas + animations (1.5 days)
- Custom `widget::canvas::Program` rendering grouped nodes
- `Animation<f32>` for opacity (insert) and pulse (update)
- Click hit-test on canvas → `Message::OpenUrl`
- Click+drag the pane (whole window is drag-handle)

### Phase 6 — Tray + settings (0.5 day)
- `tray-icon` integration: "Open settings", "Quit"
- Settings panel: repo/org list, PAT input (write to disk), save

### Phase 7 — Polish (1 day)
- 5-event-type color/icon set (PR opened = blue, PR merged = purple, issue
  opened = green, release = orange, new repo = gold)
- Humanized time range correctness for past/now/future
- Empty-state and error-state UI
- Window position persistence across restarts
- Tray icon glyph

### Phase 8 — CI (0.5 day)
- `ci.yml`: ubuntu/macos/windows matrix, `cargo fmt --check`, `cargo clippy`,
  `cargo test`, build all three release binaries
- `release.yml`: on `v*` tag, build release artifacts, attach to GitHub
  Release, generate checksums

### Phase 9 — Final review (0.5 day)
- `review-and-fix` on the whole codebase
- `ccc-review-cx` for an external Codex review
- Address findings, re-test, re-release

## Testing strategy

- **Unit tests** inside each module for pure logic
- **Snapshot tests** (`insta`) for the timeline group/compress/snapshot output.
  This is the heart of the product — snapshots let us iterate on the grouping
  rules with confidence.
- **HTTP integration tests** (`wiremock`) for the GitHub client. Verify auth
  header, polling interval, retry, response parsing, error mapping.
- **Property tests** (`proptest`) for the humanize time function.
- **End-to-end smoke test** (one test, slow) that boots the Iced app with a
  mock backend and verifies the timeline renders. Run in headless mode.
- **CI runs** `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test`, and a release build for each platform.

## CI / CD

- **`ci.yml`** — push and PR. Matrix: ubuntu-latest, macos-latest (Intel +
  Apple Silicon), windows-latest. Steps: `just fmt`, `just lint`, `just test`,
  `just build-release`. Artifacts: the release binary per OS.
- **`release.yml`** — on `v*.*.*` tag. Same matrix, but only builds release
  artifacts. Creates a GitHub Release with the binaries + checksums.

## Repo conventions

- **Format on save**, `cargo fmt --check` in CI.
- **Clippy with `-D warnings`** in CI.
- **Commit messages**: conventional-ish, present tense, no scope prefix noise.
- **Worktrees** for new features; this branch is `feat/initial-implementation`.
- **`just` for tasks**; never `cargo run` directly in the dev loop.
- **Tests live next to code** (`#[cfg(test)] mod tests`) for unit; in `tests/`
  for integration.
- **No `unwrap()` in production code**; use `?`, `anyhow`, or `thiserror`.

## Open questions / decisions to make during build

- **Tray library on Linux** — `tray-icon` uses GTK on Linux, which means GTK
  as a runtime dep. Acceptable. Document it.
- **Wayland passthrough detail** — `iced_layershell` is the official solution
  but adds a fork of Iced. Start with winit passthrough (whole-window), add
  `iced_layershell` only if needed.
- **macOS app icon** — ship a basic PNG; users can swap later.
- **Code signing** — out of scope for v1. Document in README.

## Definition of done (v1)

- [ ] `just build` produces a release binary on all three target platforms
- [ ] `just test` passes on all three platforms
- [ ] `just lint` passes
- [ ] App starts, shows a transparent overlay, hovers opaque, click+drags,
      shows a real GitHub timeline with at least one node from a real
      polled account
- [ ] Click on a node opens the URL in the default browser
- [ ] ~~App position persists across restarts~~ — v1 limitation: the
      `window_position` field is read on launch but not written back
      when the user drags the overlay. Restoring from config is fine;
      saving requires subscribing to `iced::window::events()` for
      `WindowEvent::Moved` and is deferred to v1.1.
- [ ] CI green on all three platforms
- [ ] Tagged release produces downloadable artifacts on GitHub

## v0.2.0 (added on top of v1)

- **Interactive setup wizard.** `gh-monitor init` walks the user
  through configuring their PAT (input hidden on Unix terminals via
  termios, with a warning when falling back to plain stdin), GitHub
  username, watched orgs, watched repos, and poll interval, then
  writes the validated config to the platform's user config dir.
  PAT is never echoed or logged; the wizard does not make any
  network requests.

## v0.3.0 (added on top of v0.2.0)

- **Per-source polling status.** `PollItem` now carries a
  `&'static str` source label (e.g. `"received"`, `"org/rust-lang"`,
  `"repo/octocat/Hello-World"`) so the GUI can attribute events and
  errors to a specific source. `PollStatus` in the app is no longer
  a single `Idle | Polling | Error | AuthError` enum — it tracks a
  per-source map keyed by source label, with most-recently-updated
  at the back. The status banner formats errors per source:
  `rust-lang: 401 Unauthorized` when exactly one source has erred,
  and `polling (1/3 ok)` when more than one has. The `received`
  source (no prefix) is shown as-is; the `org/` and `repo/` prefixes
  are stripped for display.

## v0.3.1 (added on top of v0.3.0)

- **Per-cycle poll application.** The v0.3.0 poller emitted one
  `Message::Polled(source, events)` per source, and the GUI rebuilt
  the snapshot from each batch. The last source polled "won" and
  every previous source's nodes were animated out, so the timeline
  flickered to a single source every cycle. Now the poller emits a
  single `Message::PolledCycle { events, errors }` per cycle,
  carrying every source's batch at once. The app flattens the events
  and rebuilds the snapshot in one shot, so all sources stay on
  screen. New unit test `polled_cycle_applies_all_sources_in_one_shot`
  exercises the regression.
- **Debounced position save.** `Message::WindowMoved` fired ~60×/sec
  during a drag, and the old handler spawned a `Task::future(save_config)`
  on every event — hundreds of disk writes per drag. Now the handler
  records `last_position_save_at` and only writes if at least 500 ms
  have passed since the last save. The in-memory config is updated
  eagerly, so the next eligible save picks up the latest position
  and no information is lost. A `config_save_pending` flag ensures
  that if the user moves, waits 400 ms, and quits, the last move is
  still flushed synchronously from `Message::Escape` and
  `TrayAction::Quit`.
- **Atomic config writes.** `save_config` used `std::fs::write` which
  truncates and writes non-atomically — a kill mid-write would leave
  a zero-byte or partial file and `load_config` would fail. Now the
  helper serializes to `<path>.toml.tmp` then `std::fs::rename`s over
  the target. `rename` is atomic on POSIX and
  `MoveFileEx(REPLACE_EXISTING)`-equivalent on Windows. A kill
  mid-write leaves the previous good file intact. A new test
  (`save_config_to_writes_final_file_atomically`) verifies that no
  stale `.tmp` is left behind after a successful save.

## v0.3.2 (added on top of v0.3.1)

- **Stricter repo validation.** `Config::validate` used
  `r.contains('/')` as a smoke test, so it accepted `"/x"`, `"x/"`,
  and `"a/b/c"` — all malformed. The poller's `intern_sources` and
  `poll_once` then desynchronised: a malformed entry was dropped in
  `poll_once` without `idx` advancing, so the next valid repo picked
  up the previous repo's source label. `Config::validate` now
  requires `owner/name` form (split on the first `/`, both halves
  non-empty, name has no further `/`); the poller's `intern_sources`
  also filters malformed repos at the source so a hand-edited config
  can't desync the labels. Validation errors are surfaced on the
  overlay's status banner via the new `POLL_CONSTRUCTION_ERROR`
  static and the existing `Message::PolledCycle` error-banner path.
- **Single-instance lockfile.** Two `gh-monitor` processes would
  both poll GitHub (2× the rate-limit pressure) and fight for the
  tray icon. `main` now takes an exclusive `flock`-style lock on
  `<config_dir>/gh-monitor.lock` before starting the GUI; a second
  instance exits with a clear "another instance of gh-monitor is
  already running; lock: <path>" message. CLI subcommands
  (`init`, `doctor`, `config`, `--version`) are unaffected and can
  run alongside the GUI. The lock is released automatically when the
  process exits (the underlying file handle is closed by `Drop` /
  the OS). The MSRV is bumped from 1.81 to 1.89 to use stable
  `std::fs::File::try_lock`.
- **Rate-limit (429) reset handling.** `ClientError::RateLimited` is
  now a struct variant carrying the `X-RateLimit-Reset` Unix
  timestamp (or a `Retry-After`-derived fallback). A new
  `rate_limit_banner` helper in `gh-monitor-gh::polling` produces
  the user-facing "rate-limited until 2024-01-15 14:30:00 UTC"
  string for the status banner. Previously a 429 produced a flat
  "rate-limited by GitHub" string with no reset hint. The poller
  still backs off 5 s on any error; a follow-up could sleep until
  the reset time.
- **Poller-construction errors surfaced.** `install_poller_if_configured`
  and the poller subscription now record errors
  (config-validation failures, `Poller::new` failures) in a
  `static POLL_CONSTRUCTION_ERROR` and emit a
  `Message::PolledCycle { errors: [("poller", err)] }` so the
  existing status banner picks them up. Previously both paths
  `warn!`-logged and returned; the user saw "nothing happens" and
  `gh-monitor doctor` reported the config as valid.

## v0.4.0 (added on top of v0.3.0)

- **Diagnostic command.** `gh-monitor doctor` runs a fixed set of
  environment checks and prints one `[ OK | WARN | FAIL ] label: detail`
  line per check (green/yellow/red on a TTY, plain otherwise). Exits
  0 if all OK, 1 if any FAIL, 2 if any WARN (no FAIL) — scriptable
  from CI. Checks: config file exists/valid TOML/`Config::validate()`
  passes; PAT non-empty; GitHub username reachable via
  `/users/{name}` (5s timeout); first org or repo reachable via
  `/orgs/{org}/events` or `/repos/{o}/{r}/events`; GTK runtime init
  on Linux; tray-icon crate data path; `DISPLAY` or `WAYLAND_DISPLAY`
  on Linux (n/a on macOS/Windows); read+write access to the config
  directory via a small probe file.

## v1.1 (deferred from v1)

- **Window position save-on-move.** Subscribe to
  `iced::window::events()` for `WindowEvent::Moved` and write the new
  position back to `Config`. v1 only restores; it doesn't save.
- **Tray / settings UI.** A clickable tray icon to open a settings
  panel (repo/org list, PAT input). v1 ships with config-from-file +
  env vars only.
- ~~**Interactive setup wizard.**~~ Shipped in v0.2.0 — see above.
