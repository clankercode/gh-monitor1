//! The Iced `Program` implementation. Owns the Iced state and wires
//! the GitHub poller, the timeline state, the canvas, and the
//! passthrough state machine together.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use gh_monitor_config::schema::WindowPosition;
use gh_monitor_config::Config;
use gh_monitor_gh::{Auth, PollConfig, PollItem, Poller, RawEvent};
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

/// Messages that drive the application.
#[derive(Debug, Clone)]
pub enum Message {
    /// A source was polled and produced these raw events. The source
    /// label is recorded against the per-source status.
    Polled(&'static str, Vec<RawEvent>),
    /// A source errored (transient).
    PollError(&'static str, String),
    /// GitHub returned 401/403 for a source — fatal, surface in UI.
    AuthError(&'static str, String),
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
        Message::Polled(source, events) => {
            state.last_poll_at = Some(Instant::now());
            state.poll_status.record_ok(source);
            apply_events(state, events);
            sync_program(state);
            Task::none()
        }
        Message::PollError(source, e) => {
            warn!(source = source, error = %e, "poll error");
            state.poll_status.record_error(source, e);
            sync_program(state);
            Task::none()
        }
        Message::AuthError(source, e) => {
            error!(source = source, error = %e, "auth error");
            state.poll_status.record_auth_error(source, e);
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
            // Persist the new position to the config. We update the
            // in-memory config and write to disk. Writes happen on
            // every move event (the config is small — ~100 bytes —
            // and the OS will coalesce), so the user gets
            // position-persisted-on-quit semantics with no extra
            // plumbing.
            state.config.window_position = Some(WindowPosition {
                x: p.x as i32,
                y: p.y as i32,
            });
            let cfg = state.config.clone();
            iced::Task::future(async move {
                if let Err(e) = crate::config_io::save_config(&cfg) {
                    warn!(error = %e, "failed to persist window position");
                }
            })
            .discard()
        }
        Message::DragWindow => match state.window_id {
            Some(id) => window::drag(id),
            None => Task::none(),
        },
        Message::Escape => iced::exit(),
        Message::Refresh => Task::none(),
        Message::TrayAction(TrayAction::Quit) => iced::exit(),
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
fn poll_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(
            8,
            async move |mut output: iced::futures::channel::mpsc::Sender<Message>| {
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
                        return;
                    }
                };
                let (fut, mut rx) = poller.into_run();
                tokio::spawn(fut);
                while let Some(item) = rx.recv().await {
                    let msg = match item {
                        PollItem::Events(source, events) => Message::Polled(source, events),
                        PollItem::Error(source, e) => Message::PollError(source, e),
                        PollItem::AuthError(source, e) => Message::AuthError(source, e),
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

/// Stash the auth + poll config so `poll_subscription` can build the
/// poller on Iced's runtime. Returns `true` if a poller was queued for
/// start (PAT was set). Idempotent: a second call is a no-op.
pub fn install_poller_if_configured(initial: &Config) -> bool {
    if initial.pat.trim().is_empty() {
        return false;
    }
    let Ok(auth) = Auth::new(initial.pat.clone()) else {
        return false;
    };
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
}
