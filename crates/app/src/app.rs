//! The Iced `Program` implementation. Owns the Iced state and wires
//! the GitHub poller, the timeline state, the canvas, and the
//! passthrough state machine together.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use gh_monitor_config::schema::WindowPosition;
use gh_monitor_config::Config;
use gh_monitor_gh::{rate_limit_banner, Auth, ClientError, PollConfig, PollItem, Poller, RawEvent};
use gh_monitor_timeline::snapshot::SnapshotDiff;
use gh_monitor_timeline::{
    compress, diff, group_by_repo, CompressionConfig, NodeId, TimelineSnapshot,
};
use iced::event::{self, Status as EventStatus};
use iced::futures::SinkExt;
use iced::keyboard::{self, key, Key};
use iced::widget::canvas::Canvas;
use iced::window::{self, Id, Level, Mode, Settings as WindowSettings};
use iced::{Element, Event, Size, Subscription, Task, Theme};
use tracing::{error, warn};

use crate::animation::NodeAnim;
use crate::canvas::TimelineProgram;
use crate::link::open_url;
use crate::overlay::OverlayState;
use crate::tray::TrayAction;

/// The settings passed in from `main.rs` to construct the app.
#[derive(Debug, Clone)]
pub struct AppSettings {
    pub initial: Config,
}

/// State held by the application.
#[derive(Debug)]
#[allow(dead_code)]
pub struct State {
    pub config: Config,
    pub snapshot: TimelineSnapshot,
    pub anims: HashMap<NodeId, NodeAnim>,
    pub overlay: OverlayState,
    /// Last time a window-move triggered a debounced config save. Used to
    /// throttle disk writes: we only persist at most once per
    /// `POSITION_SAVE_DEBOUNCE` window. The next save picks up the
    /// latest position from `state.config.window_position`, so the
    /// debounce loses no information.
    pub last_position_save_at: Option<Instant>,
    /// `true` when a `WindowMoved` event has arrived but its save was
    /// throttled. On quit (`Message::Escape` / `TrayAction::Quit`) we
    /// perform a final synchronous save if this is set, so the user
    /// doesn't lose the last move.
    pub config_save_pending: bool,
    /// Whether the cursor has entered the overlay at least once. While
    /// false, we keep the window in `Active` (non-passthrough) so the
    /// user can interact with it on first launch. The first time the
    /// cursor leaves after a hover, we transition to `Idle` and stay in
    /// the normal hover-driven passthrough mode thereafter.
    pub has_hovered: bool,
    /// Whether the window is currently hidden via `Mode::Hidden`.
    pub hidden: bool,
    pub poll_status: PollStatus,
    pub last_poll_at: Option<Instant>,
    pub program: TimelineProgram,
    pub window_id: Option<Id>,
}

/// Per-source poll status, in the order the sources were last updated.
/// The most recently updated source is at the back.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PollStatus {
    sources: Vec<SourceStatus>,
}

/// One source's poll status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStatus {
    /// Source label (e.g. `"received"`, `"org/rust-lang"`,
    /// `"repo/octocat/Hello-World"`).
    pub source: &'static str,
    pub kind: SourceStatusKind,
}

/// What state a source is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceStatusKind {
    /// The source was polled and returned events (or no events). Clears
    /// the status banner for this source.
    Ok,
    /// Transient error from this source (server error, network, rate
    /// limit). Polling continues.
    Error(String),
    /// Fatal auth error from this source. The PAT is wrong or expired.
    AuthError(String),
}

impl PollStatus {
    /// Record a successful poll from `source`. Moves an existing entry for
    /// the same source to the back, so it becomes the most-recent.
    pub(crate) fn record_ok(&mut self, source: &'static str) {
        self.sources.retain(|s| s.source != source);
        self.sources.push(SourceStatus {
            source,
            kind: SourceStatusKind::Ok,
        });
    }

    /// Record a transient error from `source`.
    pub(crate) fn record_error(&mut self, source: &'static str, msg: String) {
        self.sources.retain(|s| s.source != source);
        self.sources.push(SourceStatus {
            source,
            kind: SourceStatusKind::Error(msg),
        });
    }

    /// Record a fatal auth error from `source`.
    pub(crate) fn record_auth_error(&mut self, source: &'static str, msg: String) {
        self.sources.retain(|s| s.source != source);
        self.sources.push(SourceStatus {
            source,
            kind: SourceStatusKind::AuthError(msg),
        });
    }

    /// Number of sources tracked.
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    /// `true` if no sources have been polled yet.
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Iterate the per-source statuses (oldest first, most recent last).
    pub fn iter(&self) -> std::slice::Iter<'_, SourceStatus> {
        self.sources.iter()
    }
}

/// Format the per-source status for the canvas banner. Returns `None`
/// when nothing noteworthy has happened (no errors, or no polls yet).
///
/// Rules:
/// - All sources `Ok` (or empty) → `None`.
/// - Exactly one source has a non-`Ok` status → "`<source>`: <message>"
///   (with the `org/` / `repo/` prefix stripped for display).
/// - Two or more sources have non-`Ok` status →
///   "polling (<ok>/<total> ok)".
/// - `AuthError` is preferred over `Error` when picking the single-error
///   source.
pub(crate) fn format_poll_status(status: &PollStatus) -> Option<String> {
    if status.is_empty() {
        return None;
    }
    let errored: Vec<&SourceStatus> = status
        .iter()
        .filter(|s| !matches!(s.kind, SourceStatusKind::Ok))
        .collect();
    if errored.is_empty() {
        return None;
    }
    if errored.len() == 1 {
        let s = errored[0];
        let msg = match &s.kind {
            SourceStatusKind::Error(m) | SourceStatusKind::AuthError(m) => m.clone(),
            SourceStatusKind::Ok => unreachable!(),
        };
        return Some(format!("{}: {msg}", display_source(s.source)));
    }
    let total = status.len();
    let ok = total - errored.len();
    Some(format!("polling ({ok}/{total} ok)"))
}

/// Strip the `org/` or `repo/` prefix from a source label for display.
fn display_source(source: &str) -> &str {
    source
        .strip_prefix("org/")
        .or_else(|| source.strip_prefix("repo/"))
        .unwrap_or(source)
}

/// `true` when `err` is an auth-class error: the PAT is wrong/expired
/// (`Unauthorized`) or GitHub is throttling the user (`RateLimited`).
/// Everything else (`Server`, `Http`, `Parse`, `Events`) is treated as
/// transient — the poller will keep trying.
fn is_auth_error(err: &ClientError) -> bool {
    matches!(
        err,
        ClientError::Unauthorized(_) | ClientError::RateLimited { .. }
    )
}

/// Build the user-facing banner string for a `ClientError`. The
/// `RateLimited` variant gets the friendly "rate-limited until
/// HH:MM:SS" treatment via the shared `rate_limit_banner` helper in
/// `gh-monitor-gh`; everything else falls through to the `Display`
/// impl.
fn client_error_banner(err: &ClientError) -> String {
    match err {
        ClientError::RateLimited { .. } => rate_limit_banner(err),
        other => other.to_string(),
    }
}

/// Messages that drive the application.
#[derive(Debug, Clone)]
pub enum Message {
    /// A poll cycle completed. `events` carries every source's batch in
    /// poll order (a source with no new events is still present with an
    /// empty vec, so the per-source "OK" status is updated for it too).
    /// `errors` lists per-source errors from the same cycle, typed as
    /// [`ClientError`] so the update handler can distinguish auth
    /// (`Unauthorized`, `RateLimited`) from transient (`Server`,
    /// `Http`, `Parse`, `Events`) without string matching. The update
    /// handler applies the flattened events in one shot — emitting one
    /// message per source would re-build the snapshot per source and
    /// the last one would clobber the rest.
    PolledCycle {
        events: Vec<(&'static str, Vec<RawEvent>)>,
        errors: Vec<(&'static str, ClientError)>,
    },
    /// Cursor entered the overlay.
    HoverEntered,
    /// Cursor left the overlay.
    HoverLeft,
    /// Open a URL in the browser.
    OpenUrl(String),
    /// Window id resolved (or moved).
    WindowResolved(Id),
    /// Window was moved by the user.
    WindowMoved(iced::Point),
    /// User pressed on a non-clickable area of the pane — start dragging
    /// the window.
    DragWindow,
    /// Escape pressed — quit.
    Escape,
    /// F5 pressed — refresh now.
    Refresh,
    /// Tray menu fired an action.
    TrayAction(TrayAction),
}

/// Run the application. Blocks until the window is closed.
pub fn run(settings: AppSettings) -> anyhow::Result<()> {
    let initial = settings.initial;

    let window = WindowSettings {
        size: Size::new(420.0, 540.0),
        position: initial
            .window_position
            .map_or(window::Position::Default, |p| {
                window::Position::Specific(iced::Point::new(p.x as f32, p.y as f32))
            }),
        resizable: false,
        closeable: true,
        minimizable: true,
        decorations: false,
        transparent: true,
        level: Level::AlwaysOnTop,
        visible: true,
        ..Default::default()
    };

    // Stash the poll config so the subscription can build the poller
    // inside Iced's tokio runtime. We must NOT construct a `Poller` here
    // because `Poller::start()` calls `tokio::spawn`, which requires a
    // tokio runtime — and there is none on this thread yet.
    let _ = install_poller_if_configured(&initial);

    let result = iced::application(
        move || {
            let state = State {
                config: initial.clone(),
                snapshot: TimelineSnapshot::default(),
                anims: HashMap::new(),
                overlay: OverlayState::Active,
                last_position_save_at: None,
                config_save_pending: false,
                has_hovered: false,
                hidden: false,
                poll_status: PollStatus::default(),
                last_poll_at: None,
                program: TimelineProgram::new(),
                window_id: None,
            };
            (state, Task::none())
        },
        update,
        view,
    )
    .title(title)
    .subscription(subscription)
    .window(window)
    .theme(theme)
    .run();
    result.map_err(|e| anyhow::anyhow!("iced run: {e}"))
}

fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::PolledCycle { events, errors } => {
            // A full poll cycle completed. Apply events from every
            // source in one shot so the previous source's items aren't
            // clobbered by the next source's batch (which is what
            // happens when the snapshot is rebuilt per source).
            state.last_poll_at = Some(Instant::now());
            let mut all_events: Vec<RawEvent> = Vec::new();
            for (source, evs) in events {
                state.poll_status.record_ok(source);
                all_events.extend(evs);
            }
            for (source, e) in errors {
                let msg = client_error_banner(&e);
                if is_auth_error(&e) {
                    error!(source = source, error = %e, "auth error");
                    state.poll_status.record_auth_error(source, msg);
                } else {
                    warn!(source = source, error = %e, "poll error");
                    state.poll_status.record_error(source, msg);
                }
            }
            apply_events(state, all_events);
            sync_program(state);
            Task::none()
        }
        Message::HoverEntered => {
            state.overlay = OverlayState::Active;
            state.has_hovered = true;
            let id = state.window_id;
            id.map(window::disable_mouse_passthrough)
                .unwrap_or_else(Task::none)
        }
        Message::HoverLeft => {
            // On the first `on_exit` after a hover we transition to
            // `Idle` and enable passthrough. Before that first hover we
            // stay in `Active` so the user can interact with the overlay
            // on first launch. After that, every leave enables
            // passthrough and every enter disables it.
            let id = state.window_id;
            if state.has_hovered {
                state.overlay = OverlayState::Idle;
                id.map(window::enable_mouse_passthrough)
                    .unwrap_or_else(Task::none)
            } else {
                Task::none()
            }
        }
        Message::OpenUrl(url) => {
            open_url(&url);
            Task::none()
        }
        Message::WindowResolved(id) => {
            state.window_id = Some(id);
            state.program.window_id = Some(id);
            // Always start in `Active` (non-passthrough). The first
            // `HoverLeft` after the user hovers will switch to passthrough.
            window::disable_mouse_passthrough(id)
        }
        Message::WindowMoved(p) => {
            // Persist the new position to the config. A drag fires this
            // ~60×/sec, so we throttle disk writes to one per
            // POSITION_SAVE_DEBOUNCE. The in-memory config is updated
            // eagerly; the next debounce-eligible save picks up the
            // latest position. If a save is throttled we set
            // `config_save_pending` so the quit path can flush.
            state.config.window_position = Some(WindowPosition {
                x: p.x as i32,
                y: p.y as i32,
            });
            let now = Instant::now();
            let should_save = state
                .last_position_save_at
                .is_none_or(|t| now.duration_since(t) >= POSITION_SAVE_DEBOUNCE);
            if should_save {
                state.last_position_save_at = Some(now);
                state.config_save_pending = false;
                let cfg = state.config.clone();
                iced::Task::future(async move {
                    if let Err(e) = crate::config_io::save_config(&cfg) {
                        warn!(error = %e, "failed to persist window position");
                    }
                })
                .discard()
            } else {
                state.config_save_pending = true;
                Task::none()
            }
        }
        Message::DragWindow => match state.window_id {
            Some(id) => window::drag(id),
            None => Task::none(),
        },
        Message::Escape => {
            if state.config_save_pending {
                flush_pending_position_save(state);
            }
            iced::exit()
        }
        Message::Refresh => Task::none(),
        Message::TrayAction(TrayAction::Quit) => {
            if state.config_save_pending {
                flush_pending_position_save(state);
            }
            iced::exit()
        }
        Message::TrayAction(TrayAction::ToggleVisible) => {
            let id = state.window_id;
            if let Some(id) = id {
                state.hidden = !state.hidden;
                if state.hidden {
                    window::set_mode(id, Mode::Hidden)
                } else {
                    window::set_mode(id, Mode::Windowed)
                }
            } else {
                Task::none()
            }
        }
    }
}

/// The minimum interval between two config-on-disk writes triggered by
/// `Message::WindowMoved`. Drags emit ~60 events/sec; writing on every
/// event would do hundreds of disk writes per drag.
const POSITION_SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

/// Decide whether a `WindowMoved` should trigger a disk write given the
/// timestamp of the last successful save. Pure: extracted so the
/// debounce policy is testable without an Iced runtime.
fn should_save_position(now: Instant, last: Option<Instant>) -> bool {
    last.is_none_or(|t| now.duration_since(t) >= POSITION_SAVE_DEBOUNCE)
}

/// Synchronously flush the pending window position to disk. Called on
/// quit when a debounced save was skipped, so the user's last drag
/// position is never lost.
fn flush_pending_position_save(state: &mut State) {
    state.config_save_pending = false;
    state.last_position_save_at = Some(Instant::now());
    if let Err(e) = crate::config_io::save_config(&state.config) {
        warn!(error = %e, "failed to flush pending position save");
    }
}

fn view(state: &State) -> Element<'_, Message, Theme, iced::Renderer> {
    let canvas = Canvas::new(&state.program)
        .width(iced::Length::Fill)
        .height(iced::Length::Fill);
    let area = iced::widget::MouseArea::new(canvas)
        .on_enter(Message::HoverEntered)
        .on_exit(Message::HoverLeft)
        .on_press(Message::DragWindow);
    area.into()
}

fn subscription(_state: &State) -> Subscription<Message> {
    // Animations are read at draw time via `Animation::interpolate_with`,
    // so no per-frame tick subscription is needed.
    let kb = event::listen_with(|e, status, _id| {
        if status == EventStatus::Ignored {
            if let Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) = e {
                if matches!(key, Key::Named(key::Named::Escape)) {
                    return Some(Message::Escape);
                }
                if matches!(key, Key::Named(key::Named::F5)) {
                    return Some(Message::Refresh);
                }
            }
        }
        None
    });
    let win = window::open_events().map(Message::WindowResolved);
    let move_sub = window::events().filter_map(|(_id, ev)| {
        if let iced::window::Event::Moved(p) = ev {
            Some(Message::WindowMoved(p))
        } else {
            None
        }
    });
    let poll = poll_subscription();
    let tray = tray_subscription();
    Subscription::batch([kb, poll, win, move_sub, tray])
}

fn theme(_state: &State) -> Option<Theme> {
    Some(Theme::Dark)
}

fn title(_state: &State) -> String {
    "gh-monitor".to_string()
}

fn apply_events(state: &mut State, events: Vec<RawEvent>) {
    // A repo watched both as a user repo and via an org will surface the
    // same event id in `received_events` and `org_events`. Dedupe by id
    // before grouping so the same event doesn't inflate counts.
    let events = dedupe_events(events);
    let groups = group_by_repo(events);
    let compressed = compress(&groups, &CompressionConfig::default());
    let now = chrono::Utc::now();
    let snap = TimelineSnapshot::from_compressed(compressed, now);

    let prev = std::mem::replace(&mut state.snapshot, snap.clone());

    let d: SnapshotDiff = diff(&prev, &snap);
    let now_inst = Instant::now();
    for id in d.added {
        state.anims.insert(id, NodeAnim::new_insert(now_inst));
    }
    for id in d.updated {
        if let Some(anim) = state.anims.get_mut(&id) {
            anim.trigger_pulse(now_inst);
        } else {
            state.anims.insert(id, NodeAnim::new_insert(now_inst));
        }
    }
    let to_remove: Vec<NodeId> = state
        .anims
        .keys()
        .filter(|id| !snap.nodes.iter().any(|n| n.id == **id))
        .cloned()
        .collect();
    for id in to_remove {
        state.anims.remove(&id);
    }
}

/// Push the current state into the canvas program. Called after any
/// state change that affects the rendered view.
fn sync_program(state: &mut State) {
    state.program.snapshot = state.snapshot.clone();
    state.program.set_anims(state.anims.clone());
    state.program.needs_setup = state.config.pat.trim().is_empty();
    state.program.status = format_poll_status(&state.poll_status);
}

/// Deduplicate events by their GitHub `id`. The first occurrence wins.
fn dedupe_events(events: Vec<RawEvent>) -> Vec<RawEvent> {
    let mut seen: HashSet<String> = HashSet::with_capacity(events.len());
    let mut out: Vec<RawEvent> = Vec::with_capacity(events.len());
    for ev in events {
        if seen.insert(ev.id.clone()) {
            out.push(ev);
        }
    }
    out
}

/// The poll subscription. On first run, this constructs the poller and
/// spawns it on Iced's tokio runtime (so we don't need a separate
/// runtime). The poller then streams items into Iced via the channel.
///
/// On startup, if `install_poller_if_configured` recorded a
/// construction error (config validation failure or
/// `Poller::new` failure), we surface it as a single `PolledCycle` so
/// the existing status banner picks it up. The error is also logged at
/// WARN for the `gh-monitor doctor` / log file path.
fn poll_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(
            8,
            async move |mut output: iced::futures::channel::mpsc::Sender<Message>| {
                if let Some(err) = POLL_CONSTRUCTION_ERROR
                    .lock()
                    .ok()
                    .and_then(|mut g| g.take())
                {
                    warn!(error = %err, "poller construction failed; surfacing to UI");
                    let msg = Message::PolledCycle {
                        events: Vec::new(),
                        errors: vec![("poller", ClientError::Events(err))],
                    };
                    let _ = output.send(msg).await;
                    return;
                }
                let taken = POLL_BUILD.lock().ok().and_then(|mut g| g.take());
                let Some((auth, cfg)) = taken else {
                    // No poller was configured (e.g. PAT missing), or the
                    // subscription has already been started in a previous
                    // tick. Either way, nothing more to do.
                    return;
                };
                let poller = match Poller::new(auth, cfg) {
                    Ok(p) => p,
                    Err(e) => {
                        warn!(error = %e, "failed to build poller");
                        let msg = Message::PolledCycle {
                            events: Vec::new(),
                            // `Poller::new` returns `PollError::Auth(_)` for
                            // "no sources configured", which the
                            // poller's own run loop also maps to
                            // `ClientError::Unauthorized`. Use the same
                            // mapping here so the GUI sees a single
                            // error type.
                            errors: vec![("poller", ClientError::Unauthorized(e.to_string()))],
                        };
                        let _ = output.send(msg).await;
                        return;
                    }
                };
                let (fut, mut rx) = poller.into_run();
                tokio::spawn(fut);
                while let Some(item) = rx.recv().await {
                    let msg = match item {
                        PollItem::Cycle { events, errors } => {
                            Message::PolledCycle { events, errors }
                        }
                    };
                    if output.send(msg).await.is_err() {
                        break;
                    }
                }
            },
        )
    })
}

/// The tray subscription. Drains `crate::tray::tray_rx_owned()` and
/// turns each `TrayAction` into a `Message::TrayAction`. No-op if the
/// tray isn't started.
fn tray_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(
            4,
            async move |mut output: iced::futures::channel::mpsc::Sender<Message>| {
                let Some(rx) = crate::tray::tray_rx_owned() else {
                    warn!("tray subscription: no receiver; exiting");
                    return;
                };
                let mut rx = rx;
                while let Some(action) = rx.recv().await {
                    if output.send(Message::TrayAction(action)).await.is_err() {
                        break;
                    }
                }
            },
        )
    })
}

static POLL_BUILD: Mutex<Option<(Auth, PollConfig)>> = Mutex::new(None);

/// Construction-time error recorded by `install_poller_if_configured`
/// (config validation failure, malformed repo format, etc.). The
/// poller subscription drains this once on startup and surfaces the
/// message to the GUI as a single `PolledCycle` so the existing
/// status banner picks it up.
static POLL_CONSTRUCTION_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// Stash the auth + poll config so `poll_subscription` can build the
/// poller on Iced's runtime. Returns `true` if a poller was queued for
/// start (PAT was set and validation passed). Idempotent: a second
/// call is a no-op.
///
/// On config validation failure, the message is recorded in
/// `POLL_CONSTRUCTION_ERROR` and the function returns `false`. The
/// poller subscription will pick the message up and surface it on the
/// overlay's status banner.
pub fn install_poller_if_configured(initial: &Config) -> bool {
    if initial.pat.trim().is_empty() {
        return false;
    }
    let Ok(auth) = Auth::new(initial.pat.clone()) else {
        return false;
    };
    if let Err(e) = initial.validate() {
        if let Ok(mut g) = POLL_CONSTRUCTION_ERROR.lock() {
            *g = Some(e);
        }
        return false;
    }
    let poll_cfg = PollConfig {
        username: initial.username.clone(),
        orgs: initial.orgs.clone(),
        repos: initial.repos.clone(),
        interval: Duration::from_secs(initial.poll_interval_secs),
    };
    if let Ok(mut g) = POLL_BUILD.lock() {
        if g.is_none() {
            *g = Some((auth, poll_cfg));
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use gh_monitor_gh::{EventKind, RawEvent};

    /// Serialises the three `install_poller_*` tests so the shared
    /// `POLL_CONSTRUCTION_ERROR` and `POLL_BUILD` statics don't race
    /// with a parallel test's `install_poller_if_configured` write.
    /// Cargo runs tests in parallel within a binary by default; the
    /// lock-drain pattern alone isn't enough because the
    /// `install_poller_if_configured` call and the subsequent
    /// assertion are not atomic w.r.t. another test's call.
    static POLL_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn ev(id: &str, repo: &str, kind: EventKind, secs_ago: i64) -> RawEvent {
        let now = Utc::now();
        RawEvent {
            id: id.to_string(),
            kind,
            repo_full_name: repo.to_string(),
            created_at: now - Duration::seconds(secs_ago),
            title: None,
            url: None,
        }
    }

    #[test]
    fn dedupe_drops_duplicate_ids() {
        let e1 = ev("dup-1", "x/y", EventKind::PrOpened, 100);
        let e2 = ev("dup-1", "x/y", EventKind::PrOpened, 100);
        let e3 = ev("other-1", "x/y", EventKind::PrOpened, 50);
        let out = dedupe_events(vec![e1, e2, e3]);
        assert_eq!(out.len(), 2);
        let ids: Vec<&str> = out.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"dup-1"));
        assert!(ids.contains(&"other-1"));
    }

    #[test]
    fn dedupe_preserves_first_occurrence() {
        // The first occurrence wins; this is important because
        // `received_events` and `org_events` can return the same event
        // id with identical content — but if they differ (e.g.
        // timestamps), the first is the canonical one.
        let e1 = ev("dup-1", "x/y", EventKind::PrOpened, 100);
        let e2 = ev("dup-1", "x/y", EventKind::PrOpened, 200);
        let out = dedupe_events(vec![e1, e2]);
        assert_eq!(out.len(), 1);
        // e1 was 100s ago, e2 was 200s ago. e1's timestamp is the
        // more recent one (closer to now), and that's what we keep.
        let before = Utc::now().timestamp() - 150;
        assert!(
            out[0].created_at.timestamp() > before,
            "first occurrence (100s ago) should be kept, not the 200s-ago one"
        );
    }

    #[test]
    fn dedupe_handles_empty() {
        let out = dedupe_events(vec![]);
        assert!(out.is_empty());
    }

    #[test]
    fn poll_status_no_polls_yet_is_empty() {
        let s = PollStatus::default();
        assert!(s.is_empty());
        assert_eq!(format_poll_status(&s), None);
    }

    #[test]
    fn poll_status_all_ok_is_no_banner() {
        let mut s = PollStatus::default();
        s.record_ok("received");
        s.record_ok("org/rust-lang");
        assert_eq!(format_poll_status(&s), None);
    }

    #[test]
    fn poll_status_single_error_shows_source() {
        let mut s = PollStatus::default();
        s.record_ok("received");
        s.record_error("org/rust-lang", "401 Unauthorized".to_string());
        // Single error: show the source label (with the `org/` prefix
        // stripped) followed by the message.
        assert_eq!(
            format_poll_status(&s),
            Some("rust-lang: 401 Unauthorized".to_string())
        );
    }

    #[test]
    fn poll_status_single_auth_error_shows_source() {
        let mut s = PollStatus::default();
        s.record_auth_error("repo/octocat/Hello-World", "401".to_string());
        // `repo/` prefix is also stripped.
        assert_eq!(
            format_poll_status(&s),
            Some("octocat/Hello-World: 401".to_string())
        );
    }

    #[test]
    fn poll_status_multiple_errors_show_counts() {
        let mut s = PollStatus::default();
        s.record_ok("received");
        s.record_error("org/rust-lang", "500".to_string());
        s.record_error("repo/octocat/Hello-World", "503".to_string());
        // Two errored out of three → "polling (1/3 ok)".
        assert_eq!(format_poll_status(&s), Some("polling (1/3 ok)".to_string()));
    }

    #[test]
    fn poll_status_per_source_distinguished() {
        // Two sources with different errors: the formatting must
        // distinguish them so the user can tell which one failed.
        let mut s = PollStatus::default();
        s.record_ok("received");
        s.record_error("org/rust-lang", "401 Unauthorized".to_string());
        s.record_error("org/mozilla", "500 Server Error".to_string());
        let formatted = format_poll_status(&s);
        // With 2 errored sources we go to the counts form. To still
        // surface the per-source error, the user can hover a single
        // source — for that path, isolate one source and verify.
        assert_eq!(formatted, Some("polling (1/3 ok)".to_string()));
        // Now isolate the rust-lang source alone: confirm the per-source
        // message is preserved verbatim.
        let mut only_rust_lang = PollStatus::default();
        only_rust_lang.record_error("org/rust-lang", "401 Unauthorized".to_string());
        assert_eq!(
            format_poll_status(&only_rust_lang),
            Some("rust-lang: 401 Unauthorized".to_string())
        );
        let mut only_mozilla = PollStatus::default();
        only_mozilla.record_error("org/mozilla", "500 Server Error".to_string());
        assert_eq!(
            format_poll_status(&only_mozilla),
            Some("mozilla: 500 Server Error".to_string())
        );
    }

    #[test]
    fn poll_status_update_moves_to_most_recent() {
        let mut s = PollStatus::default();
        s.record_ok("received");
        s.record_ok("org/rust-lang");
        // Re-recording for `received` should move it to the back so
        // the most-recently updated source is last in iteration order.
        s.record_ok("received");
        let order: Vec<&'static str> = s.iter().map(|e| e.source).collect();
        assert_eq!(order, vec!["org/rust-lang", "received"]);
    }

    #[test]
    fn poll_status_source_re_recording_replaces_entry() {
        // Re-recording for the same source should not duplicate entries.
        let mut s = PollStatus::default();
        s.record_ok("received");
        s.record_error("org/rust-lang", "first".to_string());
        s.record_error("org/rust-lang", "second".to_string());
        assert_eq!(s.len(), 2, "received + rust-lang, not 3");
        assert_eq!(
            format_poll_status(&s),
            Some("rust-lang: second".to_string())
        );
    }

    #[test]
    fn should_save_position_is_true_when_never_saved() {
        // No prior save → must save.
        let now = Instant::now();
        assert!(should_save_position(now, None));
    }

    #[test]
    fn should_save_position_is_true_after_debounce_window() {
        // Last save was long enough ago that we're allowed to save again.
        let now = Instant::now();
        let last = now.checked_sub(POSITION_SAVE_DEBOUNCE + std::time::Duration::from_millis(1));
        assert!(should_save_position(now, last));
    }

    #[test]
    fn should_save_position_is_false_within_debounce_window() {
        // Last save was 100ms ago → must NOT save again.
        let now = Instant::now();
        let last = now.checked_sub(std::time::Duration::from_millis(100));
        assert!(!should_save_position(now, last));
    }

    fn test_state() -> State {
        State {
            config: Config {
                pat: "ghp_test".to_string(),
                username: Some("octocat".to_string()),
                orgs: vec!["rust-lang".to_string()],
                repos: vec![],
                poll_interval_secs: 30,
                window_position: None,
            },
            snapshot: TimelineSnapshot::default(),
            anims: HashMap::new(),
            overlay: OverlayState::Active,
            last_position_save_at: None,
            config_save_pending: false,
            has_hovered: false,
            hidden: false,
            poll_status: PollStatus::default(),
            last_poll_at: None,
            program: TimelineProgram::new(),
            window_id: None,
        }
    }

    #[test]
    fn polled_cycle_applies_all_sources_in_one_shot() {
        // Two sources with one event each. The single PolledCycle must
        // build a snapshot that contains BOTH events — the v0.3.0
        // per-source apply_events only kept the last source's batch
        // and animated the rest out.
        let mut s = test_state();
        let e1 = ev("e1", "x/y", EventKind::PrOpened, 100);
        let e2 = ev("e2", "a/b", EventKind::PrOpened, 50);
        let _ = update(
            &mut s,
            Message::PolledCycle {
                events: vec![("received", vec![e1]), ("org/rust-lang", vec![e2])],
                errors: vec![],
            },
        );
        // Both repos should appear in the snapshot.
        let repos: Vec<&str> = s.snapshot.nodes.iter().map(|n| n.repo.as_str()).collect();
        assert!(repos.contains(&"x/y"), "received event missing: {repos:?}");
        assert!(
            repos.contains(&"a/b"),
            "org event missing (regression of per-source clobber bug): {repos:?}"
        );
        // Both sources recorded as Ok.
        let sources: Vec<&'static str> = s.poll_status.iter().map(|e| e.source).collect();
        assert!(sources.contains(&"received"));
        assert!(sources.contains(&"org/rust-lang"));
    }

    #[test]
    fn polled_cycle_records_per_source_errors() {
        let mut s = test_state();
        let _ = update(
            &mut s,
            Message::PolledCycle {
                events: vec![("received", vec![]), ("org/rust-lang", vec![])],
                errors: vec![
                    (
                        "org/rust-lang",
                        ClientError::Server("500 Server Error".to_string()),
                    ),
                    (
                        "repo/octocat/Hello-World",
                        ClientError::Unauthorized("401 Unauthorized".to_string()),
                    ),
                ],
            },
        );
        // Two sources with non-Ok status → "polling (1/3 ok)".
        assert_eq!(
            format_poll_status(&s.poll_status),
            Some("polling (1/3 ok)".to_string())
        );
    }

    #[test]
    fn polled_cycle_surfaces_construction_error() {
        // The poller subscription emits a PolledCycle with a "poller"
        // source label when construction fails (e.g. config validate()
        // or Poller::new returns Err). Verify that the existing error
        // banner picks this up via the same path as a per-source error.
        let mut s = test_state();
        let _ = update(
            &mut s,
            Message::PolledCycle {
                events: vec![],
                errors: vec![(
                    "poller",
                    ClientError::Events("repo 'a/b/c' must be in 'owner/name' form".to_string()),
                )],
            },
        );
        let formatted = format_poll_status(&s.poll_status);
        assert!(
            formatted.as_deref().unwrap_or("").contains("owner/name"),
            "expected owner/name in banner, got {formatted:?}"
        );
    }

    #[test]
    fn polled_cycle_distinguishes_auth_from_transient_via_typed_errors() {
        // v1.0.1 fix: per-source errors arrive as `ClientError`, not as
        // opaque strings. The update handler must classify them by
        // variant (auth vs transient) without inspecting the display
        // string. This test feeds one auth error and one transient
        // error in the same cycle and asserts the per-source `PollStatus`
        // records each in the right kind — no `.contains("401")` or
        // similar string matching anywhere on the hot path.
        let mut s = test_state();
        let _ = update(
            &mut s,
            Message::PolledCycle {
                events: vec![("received", vec![])],
                errors: vec![
                    (
                        "org/rust-lang",
                        ClientError::Unauthorized("401 Unauthorized".to_string()),
                    ),
                    (
                        "repo/octocat/Hello-World",
                        ClientError::Server("500".to_string()),
                    ),
                ],
            },
        );
        // Pull out each source's recorded kind.
        let rust_lang: &SourceStatus = s
            .poll_status
            .iter()
            .find(|st| st.source == "org/rust-lang")
            .expect("rust-lang source should be recorded");
        let hello: &SourceStatus = s
            .poll_status
            .iter()
            .find(|st| st.source == "repo/octocat/Hello-World")
            .expect("hello-world source should be recorded");
        assert!(
            matches!(rust_lang.kind, SourceStatusKind::AuthError(_)),
            "Unauthorized must map to AuthError, got {rust_lang:?}"
        );
        assert!(
            matches!(hello.kind, SourceStatusKind::Error(_)),
            "Server must map to transient Error, got {hello:?}"
        );
    }

    #[test]
    fn window_moved_first_event_records_pending_false() {
        // First WindowMoved: last_position_save_at is None, so should_save is
        // true; config_save_pending should be cleared (we did save).
        let mut s = test_state();
        let _ = update(&mut s, Message::WindowMoved(iced::Point::new(10.0, 20.0)));
        assert!(s.last_position_save_at.is_some());
        assert!(!s.config_save_pending);
        assert_eq!(
            s.config.window_position,
            Some(WindowPosition { x: 10, y: 20 })
        );
    }

    #[test]
    fn window_moved_within_debounce_window_marks_pending() {
        // Simulate the second WindowMoved in a drag: last_position_save_at
        // is "now" so should_save is false; config_save_pending flips
        // to true and the position is still updated in memory.
        let mut s = test_state();
        s.last_position_save_at = Some(Instant::now());
        let _ = update(&mut s, Message::WindowMoved(iced::Point::new(50.0, 60.0)));
        assert!(s.config_save_pending);
        assert_eq!(
            s.config.window_position,
            Some(WindowPosition { x: 50, y: 60 })
        );
    }

    fn valid_config() -> Config {
        Config {
            pat: "ghp_test".to_string(),
            username: Some("octocat".to_string()),
            orgs: vec!["rust-lang".to_string()],
            repos: vec![],
            poll_interval_secs: 30,
            window_position: None,
        }
    }

    #[test]
    fn install_poller_records_validation_error_for_bad_repo() {
        // The three `install_poller_*` tests share the static
        // `POLL_CONSTRUCTION_ERROR` and `POLL_BUILD` and run in
        // parallel. The plain drain-at-start pattern races with the
        // other test's `install_poller_if_configured` write, so we
        // hold this lock for the entire test body to serialize them.
        let _guard = POLL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = POLL_CONSTRUCTION_ERROR
            .lock()
            .ok()
            .and_then(|mut g| g.take());
        let cfg = Config {
            repos: vec!["a/b/c".to_string()],
            ..valid_config()
        };
        assert!(!install_poller_if_configured(&cfg));
        let stored = POLL_CONSTRUCTION_ERROR.lock().ok().and_then(|g| g.clone());
        assert!(
            stored.as_deref().unwrap_or("").contains("owner/name"),
            "expected owner/name error, got {stored:?}"
        );
    }

    #[test]
    fn install_poller_records_validation_error_for_leading_slash_repo() {
        let _guard = POLL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = POLL_CONSTRUCTION_ERROR
            .lock()
            .ok()
            .and_then(|mut g| g.take());
        let cfg = Config {
            repos: vec!["/x".to_string()],
            ..valid_config()
        };
        assert!(!install_poller_if_configured(&cfg));
        let stored = POLL_CONSTRUCTION_ERROR.lock().ok().and_then(|g| g.clone());
        assert!(stored.is_some(), "expected error to be stored");
    }

    #[test]
    fn install_poller_succeeds_for_valid_config() {
        // Hold the same lock as the other two `install_poller_*`
        // tests so the shared `POLL_CONSTRUCTION_ERROR` /
        // `POLL_BUILD` statics don't race with a parallel test's
        // `install_poller_if_configured` write.
        let _guard = POLL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = POLL_CONSTRUCTION_ERROR
            .lock()
            .ok()
            .and_then(|mut g| g.take());
        let _ = POLL_BUILD.lock().ok().map(|mut g| g.take());
        let cfg = valid_config();
        assert!(install_poller_if_configured(&cfg));
        let stored = POLL_CONSTRUCTION_ERROR.lock().ok().and_then(|g| g.clone());
        assert!(
            stored.is_none(),
            "no error should be recorded for valid config, got {stored:?}"
        );
        // Drain POLL_BUILD so we don't leak state to the next test.
        let _ = POLL_BUILD.lock().ok().map(|mut g| g.take());
    }
}
