# Architecture

This document is the source of truth for module/crate responsibilities and
data flow. If you change responsibilities, update this doc.

## High-level

```
tokio runtime (Iced's)
  └─ gh::polling (background task, one tokio::spawn per cycle)
       └─ one PollItem::Cycle per cycle, carrying events + errors
              │
              ▼
        Message::PolledCycle { events, errors }
              │
              ▼
      flatten events → dedupe → group_by_repo → compress
              │
              ▼
        TimelineSnapshot + apply diff → update per-node animations
              │
              ▼
        canvas::TimelineProgram (snapshot + anims + status)
              │
              ▼
              Iced canvas::Program::draw → wgpu → window
```

## Crates

### `crates/gh` — GitHub REST client + polling

Pure logic. No GUI, no Iced dependency. Two modules: `client` (one HTTP
request, returns events) and `polling` (background task that calls
`client` on an interval and pushes results to a tokio channel).

Public API:
- `Auth` — PAT (single user). Construct once, clone freely.
- `Client` — reqwest wrapper. One per process, shared via Arc.
- `Poller::new(auth, PollConfig)` → `Poller::start()` returns
  `PollerHandle { events: mpsc::Receiver, join: JoinHandle }`.
- `PollItem::Cycle { events, errors }` — the single message type
  emitted per tick (since v0.3.1). GUI flattens the events.
- `ClientError::RateLimited { reset_at: Option<u64> }` — carries
  `X-RateLimit-Reset` so the GUI can show "rate-limited until …".

### `crates/timeline` — Group / compress / humanize

Pure functions over `Vec<RawEvent>`. Three layers:

1. `group_by_repo(events)` — produces `Vec<RepoGroup>`, one entry per
   `owner/name`, newest group first.
2. `compress(groups, config)` — collapses events in each group into
   `Vec<CompressedNode>`, splitting on time gaps. `EventKind::RepoCreated`
   is never compressed (it's a `standalone` node).
3. `TimelineSnapshot::from_compressed(compressed, now)` — computes
   humanized time labels and produces the final `Vec<TimelineNode>`.

Plus `diff(prev, next)` — produces `SnapshotDiff { added, updated, removed }`
for the renderer to drive animations. The diff uses
`TimelineNode::structural_eq` so that "59 mins ago" → "1 hr ago" label
refreshes don't pulse.

### `crates/config` — TOML config

`Config { pat, username, orgs, repos, poll_interval_secs, window_position }`.
Validated via `Config::validate()`. Strict on repo format: must be
`owner/name` (no leading/trailing slash, no nested paths). Roundtrippable
via TOML. Saved atomically via tmp + rename.

### `crates/app` — Iced binary

The `iced::application(boot, update, view)` builder wires everything
together. Key types:

- `State` — runtime state. Contains the timeline snapshot, per-node
  animations, hover state, window id, hidden flag, last position
  save timestamp, and the canvas program.
- `Message` — drives the update loop:
  `PolledCycle { events, errors } | HoverEntered | HoverLeft | OpenUrl |
   WindowResolved | WindowMoved | DragWindow | Escape | Refresh |
   TrayAction | ToggleVisible`.
- `TimelineProgram` — implements `iced::widget::canvas::Program`. The
  `draw` method paints the timeline and any status banners; `update`
  hit-tests clicks.
- `install_poller_if_configured(&Config)` — validates the config and,
  if it passes, stashes `(Auth, PollConfig)` so the poller
  subscription can build the poller inside Iced's tokio runtime.
  Records any construction error in `POLL_CONSTRUCTION_ERROR` so
  the GUI can surface it.
- `single_instance::SingleInstance` — `flock`-style exclusive lock
  on `<config_dir>/gh-monitor.lock`; refuses a second copy of the
  binary. CLI subcommands are not affected.

### State machine for hover passthrough

```
        HoverEntered              HoverLeft
Idle ────────────────► Active ────────────────► Idle
  │                     │                       │
  └─ enable_passthrough │  disable_passthrough ◄─┘
                        │
                        └─ render at full opacity
```

- The window starts in `Active` (non-passthrough) so the user can
  interact on first launch.
- The first `HoverLeft` after a hover transitions to `Idle` and
  enables passthrough. From then on, every leave enables passthrough
  and every enter disables it.

### Window visibility (Show / Hide tray item)

Tray menu's "Show / Hide" item flips `state.hidden` and calls
`window::set_mode(id, Mode::Hidden | Mode::Windowed)`. The menu label
is static; a v1.1 feature is to dynamically flip the label.

### Window position persistence

`window::events()` is filtered for `Event::Moved(Point)`. The handler
updates `state.config.window_position` eagerly, and gates the actual
disk write on a 500ms debounce (`should_save_position`). On
`Message::Escape` or `TrayAction::Quit`, if a save is pending
(`config_save_pending == true`), it's flushed synchronously.

### Animation state

Per-node `NodeAnim` holds:
- `opacity: Animation<f32>` — 0 → 1 over 400ms on insert.
- `pulse: Animation<f32>` — 0 → 1 → 0 over 600ms on update
  (auto-reversing).

Read at draw time via `opacity_at(now)` / `pulse_at(now)`. No
per-frame tick subscription is needed; `Animation::interpolate_with`
reads at draw time.

## Data flow on poll

1. The poller fetches `received_events` + per-org + per-repo events.
2. After every cycle it emits one `PollItem::Cycle { events, errors }`
   onto a tokio channel. Errors carry the source label and a string
   (typed errors are an open v1.1 item).
3. The Iced poll subscription drains that channel and produces
   `Message::PolledCycle { events, errors }`.
4. `update(PolledCycle)` flattens the events, dedupes by id, runs
   `apply_events`:
   - `group_by_repo` → `compress` → `TimelineSnapshot::from_compressed`.
   - `diff(prev, next)` produces added/updated/removed lists.
   - Per-node animations are inserted/updated/evicted.
   - Source errors are merged into `state.poll_status` (a
     `Vec<SourceStatus>`).
5. The canvas reads the snapshot + per-node animations on the next
   `draw` call.

## Threading

- The Iced event loop runs on the main thread.
- The poller runs as a `tokio::spawn` background task (spawned from
  within the Iced poll subscription's `stream::channel`).
- A second forwarder task moves events from the poller's channel into
  a global `Mutex<Option<tokio::mpsc::Receiver<PollItem>>>` that the
  Iced subscription drains.
- The tray runs on a `muda` event loop with its own thread (since
  `muda` is the GTK/Cocoa/Win32 menu event loop and can't share
  Iced's).

## What lives where

- **Pure logic** → `gh`, `timeline`, `config`. Test with `insta` +
  `wiremock` + `proptest`.
- **GUI** → `app`. Test with pure helpers (e.g. `should_save_position`,
  `empty_state_lines`) where possible. The canvas's `draw` is
  rendered through `wgpu`; a headless smoke test is a v1.1 item.
