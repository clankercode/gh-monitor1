//! The Iced `Program` implementation. Owns the Iced state and wires
//! the GitHub poller, the timeline state, the canvas, and the
//! passthrough state machine together.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

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
use iced::window::{self, Id, Level, Settings as WindowSettings};
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
    pub poll_status: PollStatus,
    pub last_poll_at: Option<Instant>,
    pub program: TimelineProgram,
    pub window_id: Option<Id>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollStatus {
    Idle,
    Polling,
    Error(String),
    AuthError(String),
}

/// Messages that drive the application.
#[derive(Debug, Clone)]
pub enum Message {
    /// A poll produced new raw events.
    Polled(Vec<RawEvent>),
    /// A poll errored (transient).
    PollError(String),
    /// GitHub returned 401/403 — fatal, surface in UI.
    AuthError(String),
    /// Cursor entered the overlay.
    HoverEntered,
    /// Cursor left the overlay.
    HoverLeft,
    /// Open a URL in the browser.
    OpenUrl(String),
    /// Window id resolved (or moved).
    WindowResolved(Id),
    /// User pressed on a non-clickable area of the pane — start dragging
    /// the window.
    DragWindow,
    /// Escape pressed — quit.
    Escape,
    /// F5 pressed — refresh now.
    Refresh,
    /// Tray menu fired an action.
    TrayAction(TrayAction),
    /// Toggle the overlay window's visibility.
    ToggleVisible,
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
                poll_status: PollStatus::Idle,
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
        Message::Polled(events) => {
            state.last_poll_at = Some(Instant::now());
            state.poll_status = PollStatus::Polling;
            apply_events(state, events);
            sync_program(state);
            Task::none()
        }
        Message::PollError(e) => {
            warn!(error = %e, "poll error");
            state.poll_status = PollStatus::Error(e);
            sync_program(state);
            Task::none()
        }
        Message::AuthError(e) => {
            error!(error = %e, "auth error");
            state.poll_status = PollStatus::AuthError(e);
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
        Message::DragWindow => match state.window_id {
            Some(id) => window::drag(id),
            None => Task::none(),
        },
        Message::Escape => iced::exit(),
        Message::Refresh => Task::none(),
        Message::TrayAction(TrayAction::Quit) => iced::exit(),
        Message::ToggleVisible => {
            // v0.1: no-op. The tray menu doesn't ship a "Show / Hide"
            // item yet, and we don't yet track a hidden flag. A real
            // implementation would call `window::set_visible(id, false)`
            // and surface a "Show" tray menu entry when hidden.
            Task::none()
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
    let poll = poll_subscription();
    let tray = tray_subscription();
    Subscription::batch([kb, poll, win, tray])
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
    state.program.status = match &state.poll_status {
        PollStatus::Idle => None,
        PollStatus::Polling => None,
        PollStatus::Error(e) => Some(format!("polling: {e}")),
        PollStatus::AuthError(e) => Some(format!("auth failed: {e}")),
    };
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
                        PollItem::Events(events) => Message::Polled(events),
                        PollItem::Error(e) => Message::PollError(e),
                        PollItem::AuthError(e) => Message::AuthError(e),
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
}
