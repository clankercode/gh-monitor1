# Architecture

This document is the source of truth for module/crate responsibilities and
data flow. If you change responsibilities, update this doc.

## High-level

```
tokio runtime
  └─ gh::polling (background task) ─► Vec<RawEvent>
                                          │
                                          ▼
                                   timeline::group
                                          │
                                          ▼
                                   timeline::compress
                                          │
                                          ▼
                                   timeline::snapshot
                                          │
                                          ▼
                              app::State.snapshot
                                          │
                                          ▼
                                 app::canvas::Program
                                          │
                                          ▼
                                  Iced Frame → wgpu
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
for the renderer to drive animations.

### `crates/config` — TOML config

`Config { pat, username, orgs, repos, poll_interval_secs, window_position }`.
Validated via `Config::validate()`. Roundtrippable via TOML.

### `crates/app` — Iced binary

The `iced::application(boot, update, view)` builder wires everything
together. Key types:

- `State` — runtime state. Contains the timeline snapshot, per-node
  animations, hover state, and the canvas program.
- `Message` — drives the update loop. Includes `Tick`, `Polled`,
  `PollError`, `AuthError`, `HoverEntered`, `HoverLeft`, `OpenUrl`,
  `WindowResolved`, `Escape`, `Refresh`.
- `TimelineProgram` — implements `iced::widget::canvas::Program`. The
  `draw` method paints the timeline; `update` hit-tests clicks.
- `install_poller_if_configured(&Config)` — sets up the tokio poller
  and wires its receiver into the Iced subscription.

### State machine for hover passthrough

```
        HoverEntered              HoverLeft
Idle ────────────────► Active ────────────────► Idle
  │                     │                       │
  └─ enable_passthrough │  disable_passthrough ◄─┘
                        │
                        └─ render at full opacity
```

- `Idle` (default): `window::enable_mouse_passthrough(id)`,
  `opacity = 0.3`.
- `Active`: `window::disable_mouse_passthrough(id)`, `opacity = 1.0`.

### Animation state

Per-node `NodeAnim` holds:
- `opacity: Animation<f32>` — 0 → 1 over 400ms on insert.
- `pulse: Animation<f32>` — 0 → 1 → 0 over 600ms on update.

Read at draw time via `opacity_at(now)` / `pulse_at(now)`. Driven by the
`window::frames()` subscription, which fires on every redraw.

## Data flow on poll

1. The poller fetches `received_events` + per-org + per-repo events.
2. Each batch is sent through a tokio channel.
3. The Iced poll subscription reads from that channel and produces
   `Message::Polled(Vec<RawEvent>)`.
4. `update(Polled)` runs `apply_events`:
   - `group_by_repo` → `compress` → `TimelineSnapshot::from_compressed`.
   - `diff(prev, next)` produces added/updated/removed lists.
   - Per-node animations are inserted/updated/evicted.
5. The canvas reads the snapshot + per-node animations on the next
   `Tick` from `window::frames()`.

## Threading

- The Iced event loop runs on the main thread.
- The poller runs as a `tokio::spawn` background task.
- A second forwarder task moves events from the poller's channel into a
  global `Mutex<Option<tokio::mpsc::Receiver<PollItem>>>` that the Iced
  subscription drains.
- All Iced drawing happens on the main thread.

## What lives where

- **Pure logic** → `gh`, `timeline`, `config`. Test with `insta` +
  `wiremock` + `proptest`.
- **GUI** → `app`. Test with the `iced` widget tests (if any) + manual
  smoke. The canvas's pure layout/URL helpers live in `app::paint` and
  are unit-tested.
