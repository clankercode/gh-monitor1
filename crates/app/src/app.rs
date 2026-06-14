//! The Iced `Program` implementation. Owns the Iced state and wires
//! the GitHub poller, the timeline state, the canvas, and the
//! passthrough state machine together.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use gh_monitor_config::schema::WindowPosition;
use gh_monitor_config::Config;
use gh_monitor_gh::{
    rate_limit_banner, Auth, ClientError, EventKind, PollConfig, Poller, RawEvent,
};
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
use tracing::{error, info, warn};

use crate::animation::NodeAnim;
use crate::canvas::TimelineProgram;
use crate::context_menu::{ContextMenu, MenuItem};
use crate::demo;
use crate::doctor::CheckResult;
use crate::link::open_url;
use crate::notifications;
use crate::overlay::OverlayState;
use crate::settings::{SettingsField, SettingsForm};
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
    /// Active demo state, if any. `Some` between the moment the user
    /// clicks the "🎬 Demo" button and the moment the
    /// `DEMO_TOTAL_SECS` window elapses; the canvas reads
    /// `remaining_secs` to render the countdown and the frame tick
    /// subscription drains due scripted events.
    pub demo: Option<demo::DemoState>,
    /// `true` when the in-pane settings panel is showing in place of
    /// the timeline. Toggled by `TrayAction::OpenSettings` and the
    /// right-click context menu's "Settings…" item.
    pub show_settings: bool,
    /// The right-click context menu, if open. `None` when dismissed.
    pub context_menu: Option<ContextMenu>,
    /// The settings form's working copy. Mirrors `state.config` while
    /// the panel is closed so a Cancel discards user edits.
    pub settings_form: SettingsForm,
    /// Last validation error from the settings form's `to_config`. Shown
    /// in the panel until the next edit or successful save.
    pub settings_error: Option<String>,
    /// `true` when the in-app Doctor diagnostics page is shown in
    /// place of the timeline. Toggled by the context menu's "Doctor…"
    /// item.
    pub show_doctor: bool,
    /// `true` when the About page is shown in place of the timeline.
    /// Toggled by the context menu's "About" item.
    pub show_about: bool,
    /// Doctor check results, populated when the user opens the
    /// Doctor page. Empty until the async check has run.
    pub doctor_results: Vec<CheckResult>,
    /// `true` while a doctor check is in flight.
    pub doctor_running: bool,
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
/// - Two or more sources AND all are `AuthError` → "all sources:
///   401 Unauthorized (check your PAT)" so the user gets a clear
///   hint that the token is the problem (not transient network).
/// - Two or more sources AND all are transient `Error` → "all
///   sources failing (network?)" so the user can distinguish "no
///   network" from "bad credentials".
/// - Exactly one source has a non-`Ok` status → "`<source>`:
///   <message>" (with the `org/` / `repo/` prefix stripped for
///   display). Single-source failures are always shown
///   per-source — the user needs to know which one failed.
/// - Two or more sources with mixed failure kinds →
///   "polling (<ok>/<total> ok)".
/// - `AuthError` is preferred over `Error` when picking the single-error
///   source.
pub(crate) fn format_poll_status(status: &PollStatus) -> Option<String> {
    if status.is_empty() {
        return None;
    }
    let total = status.len();
    let non_ok: Vec<&SourceStatus> = status
        .iter()
        .filter(|s| !matches!(s.kind, SourceStatusKind::Ok))
        .collect();
    if non_ok.is_empty() {
        return None;
    }
    // All-fail cases first, but only when there are MULTIPLE
    // sources AND every source is failing — a single source's
    // per-source message is more informative ("poller: repo
    // 'a/b/c' must be in 'owner/name' form") than the generic
    // "all sources: 401 Unauthorized", and a partially-failing
    // setup deserves the counts form.
    if non_ok.len() == total && non_ok.len() > 1 && non_ok.iter().all(|s| is_auth_source_status(s))
    {
        return Some("all sources: 401 Unauthorized (check your PAT)".to_string());
    }
    if non_ok.len() == total
        && non_ok.len() > 1
        && non_ok.iter().all(|s| is_transient_source_status(s))
    {
        return Some("all sources failing (network?)".to_string());
    }
    if non_ok.len() == 1 {
        let s = non_ok[0];
        let msg = match &s.kind {
            SourceStatusKind::Error(m) | SourceStatusKind::AuthError(m) => m.clone(),
            SourceStatusKind::Ok => unreachable!(),
        };
        return Some(format!("{}: {msg}", display_source(s.source)));
    }
    let ok = total - non_ok.len();
    Some(format!("polling ({ok}/{total} ok)"))
}

/// `true` when a `SourceStatus` is an auth-class error.
fn is_auth_source_status(s: &SourceStatus) -> bool {
    matches!(s.kind, SourceStatusKind::AuthError(_))
}

/// `true` when a `SourceStatus` is a transient (non-auth) error.
fn is_transient_source_status(s: &SourceStatus) -> bool {
    matches!(s.kind, SourceStatusKind::Error(_))
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
    /// User clicked the "🎬 Demo" button on the canvas. Replaces
    /// any in-flight demo: the timeline is cleared and a fresh
    /// `DemoState` is started.
    StartDemo,
    /// 100ms frame tick from the `iced::time::every` subscription.
    /// The handler drains due scripted events when a demo is
    /// active and otherwise no-ops. A future per-frame redraw
    /// could be wired here without needing a separate subscription.
    FrameTick(Instant),
    /// The user right-clicked the canvas. Opens a context menu at the
    /// given canvas-local position.
    OpenContextMenu(iced::Point),
    /// The user clicked a context menu item. The handler dismisses the
    /// menu and acts on the item.
    ContextMenuItem(MenuItem),
    /// The user clicked outside the context menu (or pressed Escape).
    /// Closes the menu without acting on any item.
    DismissContextMenu,
    /// The cursor moved over the canvas. Used to highlight the
    /// hovered context menu item.
    ContextMenuHover(Option<usize>),
    /// Doctor check finished; the results are ready to render.
    DoctorResults(Vec<crate::doctor::CheckResult>),
    /// User typed into one of the settings form's fields. The new
    /// value is stored verbatim; parsing / validation happens on Save.
    SettingsFieldChanged(SettingsField, String),
    /// Save the settings form: validate, write to disk, exit the panel.
    SettingsSubmit,
    /// Discard the settings form's edits and return to the timeline.
    SettingsCancel,
    /// Reset the settings form to the default config and stay in the
    /// panel.
    SettingsReset,
    /// The user clicked the bell icon in the top-right of the
    /// canvas; flip the `notifications_enabled` flag and persist
    /// the new value to disk. The settings panel's checkbox stays
    /// in sync because `state.config` is the single source of
    /// truth.
    ToggleNotifications,
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
                demo: None,
                show_settings: false,
                context_menu: None,
                settings_form: SettingsForm::from_config(&initial),
                settings_error: None,
                show_doctor: false,
                show_about: false,
                doctor_results: Vec::new(),
                doctor_running: false,
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
            let (prev_snap, diff) = apply_events(state, all_events);
            sync_program(state);
            if state.config.notifications_enabled {
                fire_notifications(&prev_snap, &state.snapshot, &diff);
            }
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
            if state.show_settings {
                cancel_settings(state);
                return Task::none();
            }
            // Escape dismisses the context menu first, then the
            // doctor/about page, and only then quits.
            if state.context_menu.is_some() {
                state.context_menu = None;
                sync_program(state);
                return Task::none();
            }
            if state.show_doctor {
                state.show_doctor = false;
                state.doctor_running = false;
                sync_program(state);
                return Task::none();
            }
            if state.show_about {
                state.show_about = false;
                sync_program(state);
                return Task::none();
            }
            if state.config_save_pending {
                flush_pending_position_save(state);
            }
            iced::exit()
        }
        Message::Refresh => {
            request_force_poll();
            Task::none()
        }
        Message::TrayAction(TrayAction::Quit) => {
            if state.config_save_pending {
                flush_pending_position_save(state);
            }
            iced::exit()
        }
        Message::TrayAction(TrayAction::OpenSettings) => {
            open_settings(state);
            Task::none()
        }
        Message::OpenContextMenu(p) => {
            state.context_menu = Some(ContextMenu::new(p));
            sync_program(state);
            Task::none()
        }
        Message::ContextMenuItem(item) => {
            state.context_menu = None;
            match item {
                MenuItem::Settings => {
                    open_settings(state);
                }
                MenuItem::ShowHide => {
                    let id = state.window_id;
                    if let Some(id) = id {
                        state.hidden = !state.hidden;
                        if state.hidden {
                            return window::set_mode(id, Mode::Hidden);
                        } else {
                            return window::set_mode(id, Mode::Windowed);
                        }
                    }
                }
                MenuItem::RefreshNow => {
                    request_force_poll();
                }
                MenuItem::Doctor => {
                    state.show_doctor = !state.show_doctor;
                    let needs_run = state.show_doctor && state.doctor_results.is_empty();
                    state.doctor_running = state.show_doctor;
                    sync_program(state);
                    if needs_run {
                        return run_doctor_async();
                    }
                }
                MenuItem::About => {
                    state.show_about = !state.show_about;
                    sync_program(state);
                }
                MenuItem::Quit => {
                    if state.config_save_pending {
                        flush_pending_position_save(state);
                    }
                    return iced::exit();
                }
                MenuItem::Separator => {}
            }
            Task::none()
        }
        Message::DismissContextMenu => {
            state.context_menu = None;
            sync_program(state);
            Task::none()
        }
        Message::ContextMenuHover(idx) => {
            if let Some(menu) = state.context_menu.as_mut() {
                menu.selected = idx;
            }
            Task::none()
        }
        Message::DoctorResults(results) => {
            state.doctor_results = results;
            state.doctor_running = false;
            sync_program(state);
            Task::none()
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
        Message::StartDemo => {
            // Reset the timeline and start a fresh demo. Subsequent
            // clicks re-run the script from a clean slate, so the
            // user can replay the demo as many times as they like.
            // `apply_events` is intentionally NOT called here — the
            // first scripted event fires at t=1.0s via the
            // `Message::FrameTick` handler.
            state.snapshot = TimelineSnapshot::default();
            state.anims.clear();
            state.demo = Some(demo::DemoState::new());
            sync_program(state);
            Task::none()
        }
        Message::FrameTick(now) => {
            // Drain due events out of the demo first so we don't
            // hold a mutable borrow on `state.demo` across the
            // `apply_events` call (which also takes `&mut state`).
            let (events, complete) = {
                let Some(active) = state.demo.as_mut() else {
                    return Task::none();
                };
                let events = active.drain_due(now);
                let complete = active.is_complete(now);
                (events, complete)
            };
            if !events.is_empty() {
                apply_events(state, events);
            }
            if complete {
                // Window elapsed — drop the demo and re-sync the
                // canvas so the indicator and `demo_remaining_secs`
                // clear. The final snapshot (with all the demo
                // nodes faded in) stays on screen.
                state.demo = None;
            }
            sync_program(state);
            Task::none()
        }
        Message::SettingsFieldChanged(field, value) => {
            if state.show_settings {
                state.settings_form.update_field(field, value);
                state.settings_error = None;
            }
            Task::none()
        }
        Message::SettingsSubmit => submit_settings(state),
        Message::SettingsCancel => {
            cancel_settings(state);
            Task::none()
        }
        Message::SettingsReset => {
            if state.show_settings {
                state.settings_form = SettingsForm::from_default();
                state.settings_error = None;
            }
            Task::none()
        }
        Message::ToggleNotifications => {
            // Flip the flag in state and persist it. The user
            // clicked the bell icon in the canvas; the icon glyph
            // is part of the canvas's draw path and reflects the
            // new value on the next paint.
            state.config.notifications_enabled = !state.config.notifications_enabled;
            // If the settings panel is open, mirror the change so
            // the checkbox stays consistent with state.config.
            if state.show_settings {
                state.settings_form = SettingsForm::from_config(&state.config);
            }
            let cfg = state.config.clone();
            sync_program(state);
            iced::Task::future(async move {
                if let Err(e) = crate::config_io::save_config(&cfg) {
                    warn!(
                        error = %e,
                        "failed to persist notifications toggle"
                    );
                }
            })
            .discard()
        }
    }
}

/// Open the in-pane settings panel. Always rebuilds the form from the
/// current config so the user sees their saved values, not a stale
/// working copy from a previous open.
fn open_settings(state: &mut State) {
    state.show_settings = true;
    state.settings_form = SettingsForm::from_config(&state.config);
    state.settings_error = None;
}

/// Cancel: discard form edits, hide the panel, leave the on-disk
/// config untouched.
fn cancel_settings(state: &mut State) {
    state.show_settings = false;
    state.settings_form = SettingsForm::from_config(&state.config);
    state.settings_error = None;
}

/// Save: validate the form, persist to disk, apply to in-memory state,
/// exit the panel. On validation failure the panel stays open and the
/// error is shown.
fn submit_settings(state: &mut State) -> Task<Message> {
    let new_cfg = match state.settings_form.to_config(&state.config) {
        Ok(cfg) => cfg,
        Err(e) => {
            state.settings_error = Some(e);
            return Task::none();
        }
    };
    if let Err(e) = crate::config_io::save_config(&new_cfg) {
        state.settings_error = Some(format!("failed to write config: {e}"));
        return Task::none();
    }
    state.config = new_cfg;
    state.show_settings = false;
    state.settings_error = None;
    let _ = install_poller_if_configured(&state.config);
    Task::none()
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

/// Fire a one-shot "poll now" signal. Consumed by the poller
/// subscription's loop via `FORCE_POLL.notified()`. Multiple clicks
/// in quick succession collapse into a single extra poll, which is
/// the correct behaviour — the loop only needs to know "as soon as
/// you're not busy, re-poll".
fn request_force_poll() {
    FORCE_POLL.notify_one();
}

#[cfg(test)]
fn test_run_doctor_async_is_noop_task() {
    let _ = run_doctor_async();
}

/// Kick off the doctor checks asynchronously and post the results
/// back as a `Message::DoctorResults`. The blocking task exists
/// because `doctor::run_all` is sync (it builds its own short-lived
/// tokio runtime for the network checks); running it on Iced's
/// tokio runtime thread would either block the runtime or panic
/// on a nested `block_on`.
fn run_doctor_async() -> Task<Message> {
    let path = crate::config_io::config_path();
    Task::perform(
        async move {
            let join = tokio::task::spawn_blocking(move || crate::doctor::run_all(&path)).await;
            match join {
                Ok(Ok(results)) => results,
                Ok(Err(e)) => vec![CheckResult::fail("doctor", format!("{e}"))],
                Err(e) => vec![CheckResult::fail(
                    "doctor",
                    format!("blocking task failed: {e}"),
                )],
            }
        },
        Message::DoctorResults,
    )
}

fn view(state: &State) -> Element<'_, Message, Theme, iced::Renderer> {
    if state.show_settings {
        settings_view(state)
    } else {
        timeline_view(state)
    }
}

fn timeline_view(state: &State) -> Element<'_, Message, Theme, iced::Renderer> {
    let canvas = Canvas::new(&state.program)
        .width(iced::Length::Fill)
        .height(iced::Length::Fill);
    // The canvas itself decides what to do with a press: publish
    // `Message::OpenUrl` for a hit-tested node, or `Message::DragWindow`
    // for empty area. Wrapping in a MouseArea for `on_press` is no
    // longer needed and was the source of the "drag only works the
    // first time" bug (the inner `and_capture` shadowed it).
    let area = iced::widget::MouseArea::new(canvas)
        .on_enter(Message::HoverEntered)
        .on_exit(Message::HoverLeft);
    area.into()
}

fn settings_view(state: &State) -> Element<'_, Message, Theme, iced::Renderer> {
    use iced::widget::{button, checkbox, column, radio, row, text, text_input, Space};

    let title = text("Settings").size(18.0);
    let pat = text_input("Personal access token", state.settings_form.pat())
        .on_input(|s| Message::SettingsFieldChanged(SettingsField::Pat, s))
        .secure(true)
        .size(13.0);
    let auth_label = text("Auth source").size(12.0);
    let pat_radio = radio(
        "PAT",
        gh_monitor_config::schema::AuthSource::Pat,
        Some(state.settings_form.auth_source()),
        |v| {
            let s = match v {
                gh_monitor_config::schema::AuthSource::Pat => "pat",
                gh_monitor_config::schema::AuthSource::Gh => "gh",
            };
            Message::SettingsFieldChanged(SettingsField::AuthSource, s.to_string())
        },
    )
    .size(13.0);
    let gh_radio = radio(
        "gh",
        gh_monitor_config::schema::AuthSource::Gh,
        Some(state.settings_form.auth_source()),
        |v| {
            let s = match v {
                gh_monitor_config::schema::AuthSource::Pat => "pat",
                gh_monitor_config::schema::AuthSource::Gh => "gh",
            };
            Message::SettingsFieldChanged(SettingsField::AuthSource, s.to_string())
        },
    )
    .size(13.0);
    let username = text_input("GitHub username", state.settings_form.username())
        .on_input(|s| Message::SettingsFieldChanged(SettingsField::Username, s))
        .size(13.0);
    let orgs = text_input("orgs (comma-separated)", state.settings_form.orgs())
        .on_input(|s| Message::SettingsFieldChanged(SettingsField::Orgs, s))
        .size(13.0);
    let repos = text_input(
        "repos (owner/name, comma-separated)",
        state.settings_form.repos(),
    )
    .on_input(|s| Message::SettingsFieldChanged(SettingsField::Repos, s))
    .size(13.0);
    let poll = text_input(
        "poll interval (seconds)",
        state.settings_form.poll_interval_secs(),
    )
    .on_input(|s| Message::SettingsFieldChanged(SettingsField::PollInterval, s))
    .size(13.0);
    let notifications = checkbox(state.settings_form.notifications_enabled())
        .label("Show system notifications on new activity")
        .on_toggle(|b| {
            Message::SettingsFieldChanged(
                SettingsField::Notifications,
                if b { "true" } else { "false" }.to_string(),
            )
        });

    let save_btn = button("Save").on_press(Message::SettingsSubmit);
    let cancel_btn = button("Cancel").on_press(Message::SettingsCancel);
    let reset_btn = button("Reset to defaults").on_press(Message::SettingsReset);

    let error_line: Element<'_, Message, Theme, iced::Renderer> = match &state.settings_error {
        Some(e) => text(e).size(12.0).into(),
        None => Space::new().height(0).into(),
    };

    column![
        title,
        Space::new().height(8),
        pat,
        auth_label,
        row![pat_radio, gh_radio].spacing(12.0),
        username,
        orgs,
        repos,
        poll,
        notifications,
        Space::new().height(4),
        error_line,
        Space::new().height(4),
        row![save_btn, cancel_btn, reset_btn].spacing(8.0),
    ]
    .spacing(6.0)
    .padding(12.0)
    .into()
}

fn subscription(_state: &State) -> Subscription<Message> {
    // Animations are read at draw time via `Animation::interpolate_with`,
    // so no per-frame redraw subscription is needed. The frame tick
    // below fires every 100ms and is what the demo scheduler uses to
    // drain due scripted events; it is a no-op when no demo is
    // active, so the subscription's baseline cost is a single
    // `Instant::now()` every 100ms.
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
    let frame = frame_subscription();
    let poll = poll_subscription();
    let tray = tray_subscription();
    Subscription::batch([kb, frame, poll, win, move_sub, tray])
}

/// 100ms frame tick. The handler for `Message::FrameTick` drains
/// due demo events when one is active; otherwise the message is a
/// no-op. The 100ms cadence is fine for the demo (events fire on
/// whole-second boundaries, so we get up to ten checks per
/// scheduled event). A future per-frame redraw could share this
/// subscription without any API change.
fn frame_subscription() -> Subscription<Message> {
    iced::time::every(Duration::from_millis(100)).map(Message::FrameTick)
}

fn theme(_state: &State) -> Option<Theme> {
    Some(Theme::Dark)
}

fn title(_state: &State) -> String {
    "gh-monitor".to_string()
}

fn apply_events(state: &mut State, events: Vec<RawEvent>) -> (TimelineSnapshot, SnapshotDiff) {
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
    for id in &d.added {
        state
            .anims
            .insert(id.clone(), NodeAnim::new_insert(now_inst));
    }
    for id in &d.updated {
        if let Some(anim) = state.anims.get_mut(id) {
            anim.trigger_pulse(now_inst);
        } else {
            state
                .anims
                .insert(id.clone(), NodeAnim::new_insert(now_inst));
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
    (prev, d)
}

/// Push the current state into the canvas program. Called after any
/// state change that affects the rendered view.
fn sync_program(state: &mut State) {
    state.program.snapshot = state.snapshot.clone();
    state.program.set_anims(state.anims.clone());
    state.program.needs_setup = state.config.pat.trim().is_empty();
    state.program.status = format_poll_status(&state.poll_status);
    state.program.demo_remaining_secs = state
        .demo
        .as_ref()
        .map(|d| d.remaining_secs(Instant::now()));
    state.program.notifications_enabled = state.config.notifications_enabled;
    state.program.context_menu = state.context_menu.clone();
    state.program.show_doctor = state.show_doctor;
    state.program.show_about = state.show_about;
    state.program.doctor_results = state.doctor_results.clone();
    state.program.doctor_running = state.doctor_running;
}

/// Compute the per-(repo, kind) count delta introduced by `diff`.
/// Added nodes contribute their full `(kind, count)` pairs (the
/// node did not exist before). Updated nodes contribute the count
/// delta only — if the kind set changed (e.g. an "issues opened"
/// pair was added on top of "PRs opened"), the new pair's full
/// count is added because the previous node had no entry for it.
///
/// Pure helper extracted for testability: tests can drive
/// `aggregate_new_counts` directly with crafted snapshots and
/// diffs to verify the aggregation rules without going through
/// the Iced runtime.
fn aggregate_new_counts(
    prev: &TimelineSnapshot,
    next: &TimelineSnapshot,
    diff: &SnapshotDiff,
) -> BTreeMap<(String, EventKind), u32> {
    let prev_by_id: HashMap<&NodeId, &gh_monitor_timeline::TimelineNode> =
        prev.nodes.iter().map(|n| (&n.id, n)).collect();
    let next_by_id: HashMap<&NodeId, &gh_monitor_timeline::TimelineNode> =
        next.nodes.iter().map(|n| (&n.id, n)).collect();
    let mut out: BTreeMap<(String, EventKind), u32> = BTreeMap::new();
    for id in &diff.added {
        if let Some(node) = next_by_id.get(id) {
            for (kc,) in &node.pairs {
                *out.entry((node.repo.clone(), kc.kind)).or_insert(0) += kc.count;
            }
        }
    }
    for id in &diff.updated {
        let Some(node) = next_by_id.get(id) else {
            continue;
        };
        let prev_node = prev_by_id.get(id);
        let prev_pairs: HashMap<EventKind, u32> = prev_node
            .map(|n| n.pairs.iter().map(|(kc,)| (kc.kind, kc.count)).collect())
            .unwrap_or_default();
        for (kc,) in &node.pairs {
            let prev_count = prev_pairs.get(&kc.kind).copied().unwrap_or(0);
            let delta = kc.count.saturating_sub(prev_count);
            if delta > 0 {
                *out.entry((node.repo.clone(), kc.kind)).or_insert(0) += delta;
            }
        }
    }
    out
}

/// Fire one notification per non-zero (repo, kind) pair, in
/// sorted order so two cycles with the same activity produce the
/// same notification order. Best-effort: a single failure is
/// logged and the rest still fire.
fn fire_notifications(prev: &TimelineSnapshot, next: &TimelineSnapshot, diff: &SnapshotDiff) {
    let counts = aggregate_new_counts(prev, next, diff);
    for ((repo, kind), count) in counts {
        if count == 0 {
            continue;
        }
        let body = format!("{repo}: {}", crate::paint::pair_label(kind, count));
        notifications::fire("gh-monitor", &body);
    }
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

/// The poll subscription. Subscribes to the `POLL_CONFIG` watch
/// channel and rebuilds the poller whenever the user changes the
/// config via the settings panel. Runs the poller on Iced's tokio
/// runtime (so we don't need a separate runtime) and streams
/// results into Iced via the channel.
///
/// Startup: if `install_poller_if_configured` recorded a
/// construction error (config validation failure or
/// `Poller::new` failure), we surface it as a single `PolledCycle`
/// so the existing status banner picks it up. The error is also
/// logged at WARN for the `gh-monitor doctor` / log file path.
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
                // Subscribe to the live config channel. The
                // initial value is whatever `install_poller_*`
                // last published (or `None` if it never ran).
                let mut rx = POLL_CONFIG.0.subscribe();
                // Mark the current value as seen so the
                // `has_changed()` check below only fires on
                // *future* config writes, not the initial state.
                let initial: Option<(Auth, PollConfig)> = rx.borrow_and_update().clone();
                let mut poller: Option<Poller> = match initial.as_ref() {
                    Some((auth, cfg)) => match Poller::new(auth.clone(), cfg.clone()) {
                        Ok(p) => Some(p),
                        Err(e) => {
                            warn!(error = %e, "failed to build initial poller");
                            let msg = Message::PolledCycle {
                                events: Vec::new(),
                                errors: vec![("poller", ClientError::Unauthorized(e.to_string()))],
                            };
                            let _ = output.send(msg).await;
                            None
                        }
                    },
                    None => None,
                };
                // Build the initial ticker from the initial
                // interval (or a sensible default if no poller
                // is configured yet). The ticker is recreated
                // whenever the interval changes.
                let initial_interval = initial
                    .as_ref()
                    .map(|(_, c)| c.interval)
                    .unwrap_or(DEFAULT_POLL_INTERVAL);
                let mut ticker = tokio::time::interval(initial_interval);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    // Check for a config change BEFORE waiting
                    // for the next tick. `has_changed()` is
                    // non-blocking and returns true exactly
                    // once per `send()` (after the receiver's
                    // last `borrow_and_update()`). The
                    // `RecvError` variant only fires when the
                    // sender is dropped, which would mean the
                    // process is shutting down — fall through
                    // to the normal tick path in that case.
                    if rx.has_changed().unwrap_or(false) {
                        let new = rx.borrow_and_update().clone();
                        match new {
                            Some((auth, cfg)) => {
                                info!(
                                    interval = ?cfg.interval,
                                    "poller config changed; rebuilding"
                                );
                                match Poller::new(auth, cfg.clone()) {
                                    Ok(p) => {
                                        poller = Some(p);
                                        // Recreate the ticker so
                                        // the new interval takes
                                        // effect. The first tick
                                        // of a fresh `interval`
                                        // is immediate; that's
                                        // the desired behaviour
                                        // (the user just changed
                                        // settings, give them a
                                        // fresh poll).
                                        ticker = tokio::time::interval(cfg.interval);
                                        ticker.set_missed_tick_behavior(
                                            tokio::time::MissedTickBehavior::Skip,
                                        );
                                    }
                                    Err(e) => {
                                        warn!(error = %e, "failed to rebuild poller");
                                        poller = None;
                                        let msg = Message::PolledCycle {
                                            events: Vec::new(),
                                            errors: vec![(
                                                "poller",
                                                ClientError::Unauthorized(e.to_string()),
                                            )],
                                        };
                                        let _ = output.send(msg).await;
                                    }
                                }
                            }
                            None => {
                                info!("poller config cleared; dropping running poller");
                                poller = None;
                            }
                        }
                        continue;
                    }
                    // No config change — wait for the next
                    // tick, a forced poll, or a config change
                    // (whichever comes first).
                    let notified = FORCE_POLL.notified();
                    tokio::pin!(notified);
                    let tick = ticker.tick();
                    tokio::pin!(tick);
                    let changed = rx.changed();
                    tokio::pin!(changed);
                    let _reason: &'static str;
                    tokio::select! {
                        _ = &mut tick => { _reason = "interval"; }
                        _ = notified => { _reason = "forced"; }
                        _ = &mut changed => {
                            // Config changed; loop back to the
                            // top and rebuild before polling.
                            continue;
                        }
                    }
                    let Some(poller) = poller.as_ref() else {
                        // No poller (PAT cleared). Drop the
                        // tick and wait for the next event.
                        continue;
                    };
                    let outcome = poller.poll_once().await;
                    let mut events_out: Vec<(&'static str, Vec<RawEvent>)> =
                        Vec::with_capacity(outcome.sources.len());
                    let mut errors_out: Vec<(&'static str, ClientError)> = Vec::new();
                    for source in outcome.sources {
                        for err in source.errors {
                            let client_err = match err {
                                gh_monitor_gh::PollError::Client(c) => c,
                                gh_monitor_gh::PollError::Auth(s) => ClientError::Unauthorized(s),
                            };
                            errors_out.push((source.source, client_err));
                        }
                        events_out.push((source.source, source.events));
                    }
                    let msg = Message::PolledCycle {
                        events: events_out,
                        errors: errors_out,
                    };
                    let _ = _reason; // kept for future log use
                    if output.send(msg).await.is_err() {
                        break;
                    }
                }
            },
        )
    })
}

/// Fallback poll interval used by the poller subscription when no
/// config is installed yet (e.g. fresh launch with no PAT). The
/// user-facing default lives in `gh-monitor-config`; this constant
/// is just a sane placeholder for the ticker's initial schedule.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(600);

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

/// One-shot wake-up for the poller subscription. `notify_one()`
/// posts a single permit; the subscription's `notified().await`
/// consumes it. Storing it in a `LazyLock` keeps the construction
/// off the hot path.
static FORCE_POLL: std::sync::LazyLock<tokio::sync::Notify> =
    std::sync::LazyLock::new(tokio::sync::Notify::new);

/// Construction-time error recorded by `install_poller_if_configured`
/// (config validation failure, malformed repo format, etc.). The
/// poller subscription drains this once on startup and surfaces the
/// message to the GUI as a single `PolledCycle` so the existing
/// status banner picks it up.
static POLL_CONSTRUCTION_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// The payload broadcast on the poller config watch channel. The
/// `Option` lets a settings save that clears the PAT also tear
/// down the running poller (`None` ⇒ "drop, wait for a new
/// value").
type PollConfigPayload = Option<(Auth, PollConfig)>;

/// Live `(Auth, PollConfig)` broadcast for the poller subscription.
/// `install_poller_if_configured` writes to this whenever the user
/// saves a new config (startup, settings save, PAT change); the
/// poller subscription holds a `watch::Receiver` and rebuilds
/// itself with the new config whenever the value changes. The
/// `POLL_CONFIG` watch channel replaces an earlier
/// `Mutex<Option<…>>` pattern, which was consumed once on first use
/// and never updated, so post-startup config changes (the v1.1
/// settings flow) never reached the running poller.
///
/// The channel is initialised with `None`, which matches the
/// previous startup behaviour (no poller until a config is
/// installed).
///
/// The second element of the tuple is a "keeper" `watch::Receiver`
/// that we never read from — it exists solely to keep the channel
/// open. Without it, `Sender::send` would return `Err` (and drop
/// the value) whenever no live subscription is holding a receiver
/// (e.g. in tests, or during the gap between an Iced restart and
/// the subscription factory running again).
static POLL_CONFIG: std::sync::LazyLock<(
    tokio::sync::watch::Sender<PollConfigPayload>,
    tokio::sync::watch::Receiver<PollConfigPayload>,
)> = std::sync::LazyLock::new(|| {
    let (tx, rx) = tokio::sync::watch::channel(None);
    (tx, rx)
});

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
        // No PAT: signal the poller subscription to drop any
        // running poller. The earlier `Mutex<Option<…>>` pattern
        // returned early in this branch; the `POLL_CONFIG` watch
        // channel actively publishes `None` so a settings save
        // that clears the PAT also tears down the live poller.
        let _ = POLL_CONFIG.0.send(None);
        return false;
    }
    let Ok(auth) = Auth::new(initial.pat.clone()) else {
        let _ = POLL_CONFIG.0.send(None);
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
    // `watch::Sender::send` is non-blocking and returns `Err`
    // only when there are no receivers. The poller subscription
    // always holds a receiver, so an error here is a logic bug
    // — log and move on.
    if let Err(e) = POLL_CONFIG.0.send(Some((auth, poll_cfg))) {
        warn!(error = %e, "failed to broadcast poller config; no receiver");
    }
    true
}

/// Read the current value of the live poller-config watch channel.
/// Returns the latest `(Auth, PollConfig)` published by
/// `install_poller_if_configured`, or `None` if no PAT is set or the
/// channel has been cleared. Exposed for tests so they can assert
/// the side-effect of a settings save.
#[cfg(test)]
fn current_poll_config() -> Option<(Auth, PollConfig)> {
    POLL_CONFIG.0.subscribe().borrow().clone()
}

/// Reset the live poller-config watch channel to `None`. Used by
/// tests for setup/teardown so a `send(None)` from a previous test
/// doesn't bleed into the next one. Production code never calls
/// this; in production the channel is updated only by
/// `install_poller_if_configured`.
#[cfg(test)]
fn reset_poll_config() {
    let _ = POLL_CONFIG.0.send(None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, Utc};
    use gh_monitor_gh::{EventKind, RawEvent};

    /// Serialises the three `install_poller_*` tests so the shared
    /// `POLL_CONSTRUCTION_ERROR` static and the `POLL_CONFIG` watch
    /// channel don't race with a parallel test's
    /// `install_poller_if_configured` write. Cargo runs tests in
    /// parallel within a binary by default; the lock-drain pattern
    /// alone isn't enough because the `install_poller_if_configured`
    /// call and the subsequent assertion are not atomic w.r.t.
    /// another test's call.
    static POLL_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn ev(id: &str, repo: &str, kind: EventKind, secs_ago: i64) -> RawEvent {
        let now = Utc::now();
        RawEvent {
            id: id.to_string(),
            kind,
            repo_full_name: repo.to_string(),
            created_at: now - ChronoDuration::seconds(secs_ago),
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
    fn poll_status_all_auth_returns_helpful_message() {
        // When every source is an auth error the user has a token
        // problem, not a network problem. The uninformative
        // "polling (0/N ok)" must NOT appear; instead the banner
        // must call out the PAT explicitly.
        let mut s = PollStatus::default();
        s.record_auth_error("org/rust-lang", "401".to_string());
        s.record_auth_error("repo/octocat/Hello-World", "401".to_string());
        s.record_auth_error("received", "401".to_string());
        assert_eq!(
            format_poll_status(&s),
            Some("all sources: 401 Unauthorized (check your PAT)".to_string())
        );
    }

    #[test]
    fn poll_status_all_transient_returns_helpful_message() {
        // When every source is a transient (non-auth) error, the
        // user has a network problem, not a credentials problem.
        let mut s = PollStatus::default();
        s.record_error("org/rust-lang", "500 Server Error".to_string());
        s.record_error("repo/octocat/Hello-World", "503".to_string());
        assert_eq!(
            format_poll_status(&s),
            Some("all sources failing (network?)".to_string())
        );
    }

    #[test]
    fn poll_status_mixed_falls_through_to_counts() {
        // A mix of auth and transient errors is NOT an
        // all-auth case (some are transient). The uninformative
        // counts form is acceptable here.
        let mut s = PollStatus::default();
        s.record_ok("received");
        s.record_auth_error("org/rust-lang", "401".to_string());
        s.record_error("repo/octocat/Hello-World", "500".to_string());
        // 1 ok, 2 errored (mixed kinds) → counts form.
        assert_eq!(format_poll_status(&s), Some("polling (1/3 ok)".to_string()));
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
        let config = Config {
            pat: "ghp_test".to_string(),
            username: Some("octocat".to_string()),
            orgs: vec!["rust-lang".to_string()],
            repos: vec![],
            poll_interval_secs: 30,
            ..Config::default()
        };
        let form = SettingsForm::from_config(&config);
        State {
            config,
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
            demo: None,
            context_menu: None,
            show_doctor: false,
            show_about: false,
            doctor_results: Vec::new(),
            doctor_running: false,
            show_settings: false,
            settings_form: form,
            settings_error: None,
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
            ..Config::default()
        }
    }

    #[test]
    fn install_poller_records_validation_error_for_bad_repo() {
        // The three `install_poller_*` tests share the static
        // `POLL_CONSTRUCTION_ERROR` and the `POLL_CONFIG` watch
        // channel and run in parallel. The plain drain-at-start
        // pattern races with the other test's
        // `install_poller_if_configured` write, so we hold this
        // lock for the entire test body to serialize them.
        let _guard = POLL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = POLL_CONSTRUCTION_ERROR
            .lock()
            .ok()
            .and_then(|mut g| g.take());
        reset_poll_config();
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
        reset_poll_config();
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
        // `POLL_CONFIG` statics don't race with a parallel
        // test's `install_poller_if_configured` write.
        let _guard = POLL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = POLL_CONSTRUCTION_ERROR
            .lock()
            .ok()
            .and_then(|mut g| g.take());
        reset_poll_config();
        let cfg = valid_config();
        assert!(install_poller_if_configured(&cfg));
        let stored = POLL_CONSTRUCTION_ERROR.lock().ok().and_then(|g| g.clone());
        assert!(
            stored.is_none(),
            "no error should be recorded for valid config, got {stored:?}"
        );
        // The new value must be visible on the live config
        // channel — this is the v1.1 fix: the running poller can
        // pick up post-startup config changes.
        let live = current_poll_config().expect("config must be published");
        assert_eq!(live.0.pat, cfg.pat, "live PAT must match the saved PAT");
        assert_eq!(live.1.interval, Duration::from_secs(cfg.poll_interval_secs));
        assert_eq!(live.1.orgs, cfg.orgs);
        // Reset so we don't leak state to the next test.
        reset_poll_config();
    }

    #[test]
    fn submit_settings_updates_poller_config() {
        // User-visible path: the user opens settings, changes the
        // PAT/orgs/poll-interval, hits Save, and the change must
        // reach the live poller. `submit_settings` calls
        // `install_poller_if_configured` which publishes on the
        // `POLL_CONFIG` watch channel; before the v1.1 fix that
        // function wrote to a `Mutex<Option<…>>` that was drained
        // once and never re-read, so post-startup config changes
        // were silently dropped.
        let _guard = POLL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = POLL_CONSTRUCTION_ERROR
            .lock()
            .ok()
            .and_then(|mut g| g.take());
        reset_poll_config();
        // Pre-condition: starting config (with the default
        // 30-second interval from `valid_config`) is published.
        let initial = valid_config();
        assert!(install_poller_if_configured(&initial));
        let before = current_poll_config().expect("initial config must publish");
        assert_eq!(before.1.interval, Duration::from_secs(30));
        assert_eq!(before.1.orgs, initial.orgs);

        // Now simulate a settings save: a new config with a
        // different poll interval, a new org, and a new repo.
        // `submit_settings` itself goes through
        // `crate::config_io::save_config` (which writes to disk
        // and is not mockable in unit tests), so we exercise the
        // exact same `install_poller_if_configured` call that
        // `submit_settings` makes — this is the function whose
        // contract changed in v1.1.
        let updated = Config {
            pat: "ghp_test".to_string(),
            username: Some("octocat".to_string()),
            orgs: vec!["tokio-rs".to_string()],
            repos: vec!["rust-lang/rust".to_string()],
            poll_interval_secs: 120,
            ..valid_config()
        };
        assert!(install_poller_if_configured(&updated));
        let after = current_poll_config().expect("updated config must publish");
        assert_eq!(
            after.1.interval,
            Duration::from_secs(120),
            "new poll interval must be visible on the live channel"
        );
        assert_eq!(
            after.1.orgs,
            vec!["tokio-rs".to_string()],
            "new orgs must be visible on the live channel"
        );
        assert_eq!(
            after.1.repos,
            vec!["rust-lang/rust".to_string()],
            "new repos must be visible on the live channel"
        );
        // Cleanup.
        reset_poll_config();
    }

    #[tokio::test]
    async fn poll_subscription_picks_up_new_config() {
        // v1.1 regression: a running poller must observe a
        // post-startup config change and re-build itself. The
        // full `poll_subscription` factory spawns on Iced's
        // runtime; we test the watch-channel contract directly so
        // the test stays fast and doesn't need a renderer.
        //
        // 1. Subscribe to the live channel and mark the initial
        //    value as seen (matching what `poll_subscription`
        //    does on startup).
        // 2. Send a new config via `install_poller_if_configured`.
        // 3. Assert the receiver observes the change.
        let _guard = POLL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = POLL_CONSTRUCTION_ERROR
            .lock()
            .ok()
            .and_then(|mut g| g.take());
        reset_poll_config();

        let mut rx = POLL_CONFIG.0.subscribe();
        // Mark the initial value as seen.
        let _ = rx.borrow_and_update();
        // Pre-condition: no change observed yet.
        assert!(
            !rx.has_changed().unwrap_or(false),
            "no change should be observed before any send"
        );

        // Send a new config — the running poller would now
        // re-build itself with this value.
        let cfg = valid_config();
        assert!(install_poller_if_configured(&cfg));
        assert!(
            rx.has_changed().unwrap_or(false),
            "a new config send must be observed by the receiver"
        );
        let observed = rx
            .borrow_and_update()
            .clone()
            .expect("value must be Some after a successful install");
        assert_eq!(observed.0.pat, cfg.pat);
        assert_eq!(
            observed.1.interval,
            Duration::from_secs(cfg.poll_interval_secs)
        );
        assert_eq!(observed.1.orgs, cfg.orgs);

        // Second send — change the interval; the running poller
        // would reset its ticker. Verify the channel still works.
        let cfg2 = Config {
            poll_interval_secs: 60,
            ..cfg.clone()
        };
        assert!(install_poller_if_configured(&cfg2));
        assert!(
            rx.has_changed().unwrap_or(false),
            "a second config send must also be observed"
        );
        let observed2 = rx.borrow_and_update().clone().expect("value must be Some");
        assert_eq!(observed2.1.interval, Duration::from_secs(60));

        // Clear the PAT — this is the v1.1 path for "user
        // emptied the PAT field in settings". The running poller
        // would drop itself.
        let empty_pat = Config {
            pat: String::new(),
            ..cfg.clone()
        };
        assert!(!install_poller_if_configured(&empty_pat));
        assert!(
            rx.has_changed().unwrap_or(false),
            "a clear (PAT emptied) must be observed"
        );
        let observed3 = rx.borrow_and_update().clone();
        assert!(
            observed3.is_none(),
            "clearing the PAT must publish None, got {observed3:?}"
        );

        // Cleanup.
        reset_poll_config();
    }

    // === Demo mode tests =========================================

    #[test]
    fn start_demo_clears_snapshot_and_installs_state() {
        // A user-visible start: a stale snapshot must be wiped and
        // `state.demo` must be populated. We assert the post-state
        // without going through Iced.
        let mut s = test_state();
        // Pre-populate the snapshot with a node so we can prove the
        // clear works.
        let dirty = RawEvent::for_test(
            "old/repo".to_string(),
            EventKind::PrOpened,
            chrono::Utc::now(),
        );
        apply_events(&mut s, vec![dirty]);
        assert!(
            !s.snapshot.nodes.is_empty(),
            "precondition: snapshot is dirty"
        );

        let _ = update(&mut s, Message::StartDemo);
        assert!(
            s.snapshot.nodes.is_empty(),
            "StartDemo must clear the snapshot"
        );
        assert!(s.anims.is_empty(), "StartDemo must clear the animations");
        let demo = s.demo.as_ref().expect("StartDemo must install a DemoState");
        assert_eq!(demo.len(), 10, "demo script has 10 scheduled events");
    }

    #[test]
    fn demo_tick_applies_due_events() {
        // User spec: feed fake times into the FrameTick handler and
        // assert the snapshot updates. We use `new_at` so the test
        // owns the demo's `started_at` instant and can pin the
        // events at exact wall-clock offsets.
        let mut s = test_state();
        let t0 = Instant::now();
        s.demo = Some(crate::demo::DemoState::new_at(t0));

        // Tick at t=0.5s — the first scheduled event is at t=1.0s,
        // so nothing should fire.
        let _ = update(&mut s, Message::FrameTick(t0 + Duration::from_millis(500)));
        assert!(
            s.snapshot.nodes.is_empty(),
            "no events should fire before t=1.0s, got nodes: {:?}",
            s.snapshot.nodes
        );

        // Tick at t=1.0s — first event fires; rust-lang/rust node
        // appears in the snapshot.
        let _ = update(&mut s, Message::FrameTick(t0 + Duration::from_secs(1)));
        let repos: Vec<&str> = s.snapshot.nodes.iter().map(|n| n.repo.as_str()).collect();
        assert!(
            repos.contains(&"rust-lang/rust"),
            "expected rust-lang/rust after first demo event, got {repos:?}"
        );
        assert_eq!(
            s.snapshot.nodes.len(),
            1,
            "only one demo event has fired, got {} nodes",
            s.snapshot.nodes.len()
        );

        // Tick at t=4.0s — the first three PRs (rust-lang/rust,
        // tokio-rs/tokio, two more rust-lang/rust pulses) have
        // fired. The rust-lang/rust node should now have count=3
        // for PrOpened; tokio-rs/tokio should have count=1.
        let _ = update(&mut s, Message::FrameTick(t0 + Duration::from_secs(4)));
        let repos: Vec<&str> = s.snapshot.nodes.iter().map(|n| n.repo.as_str()).collect();
        assert!(
            repos.contains(&"rust-lang/rust"),
            "expected rust-lang/rust in snapshot, got {repos:?}"
        );
        assert!(
            repos.contains(&"tokio-rs/tokio"),
            "expected tokio-rs/tokio in snapshot, got {repos:?}"
        );
        // Animation state must have been created for the new node.
        assert!(
            !s.anims.is_empty(),
            "FrameTick should trigger animation inserts"
        );
    }

    #[test]
    fn demo_state_clears_after_completion() {
        // After `DEMO_TOTAL_SECS` the demo state must be dropped and
        // the canvas's `demo_remaining_secs` must clear, but the
        // final snapshot (the timeline with all the demo nodes)
        // stays visible.
        let mut s = test_state();
        let t0 = Instant::now();
        s.demo = Some(crate::demo::DemoState::new_at(t0));

        // Tick past the demo window.
        let far = t0 + Duration::from_secs(crate::demo::DEMO_TOTAL_SECS) + Duration::from_secs(5);
        let _ = update(&mut s, Message::FrameTick(far));
        assert!(
            s.demo.is_none(),
            "demo state must be cleared after the window elapses"
        );
        // The canvas's remaining-seconds counter must clear too, so
        // the indicator disappears on the next draw.
        assert!(
            s.program.demo_remaining_secs.is_none(),
            "demo_remaining_secs on the canvas program must be None after completion"
        );
    }

    #[test]
    fn frame_tick_is_noop_when_no_demo_active() {
        // A FrameTick without a demo must not touch the snapshot or
        // any other state. This is the steady-state path the
        // subscription runs at 10Hz for the entire app lifetime.
        let mut s = test_state();
        let before_nodes = s.snapshot.nodes.len();
        let _ = update(&mut s, Message::FrameTick(Instant::now()));
        assert_eq!(s.snapshot.nodes.len(), before_nodes);
    }

    // Notifications tests. All three flip the global
    // `notifications::CAPTURE_MODE` so they serialise on
    // `notifications::TEST_LOCK` to avoid clobbering each other's
    // captured notifications.
    #[test]
    fn notifications_disabled_does_not_fire() {
        let _guard = crate::notifications::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::notifications::set_capture_mode(true);
        let _ = crate::notifications::take_captured();

        // notifications_enabled defaults to false; feeding a
        // PolledCycle must NOT push anything to the capture
        // buffer.
        let mut s = test_state();
        assert!(!s.config.notifications_enabled);
        let e1 = ev("e1", "rust-lang/rust", EventKind::PrOpened, 100);
        let _ = update(
            &mut s,
            Message::PolledCycle {
                events: vec![("received", vec![e1])],
                errors: vec![],
            },
        );
        let captured = crate::notifications::take_captured();
        crate::notifications::set_capture_mode(false);
        assert!(
            captured.is_empty(),
            "no notification should fire when disabled, got {captured:?}"
        );
    }

    #[test]
    fn notification_aggregates_per_repo_and_kind() {
        // 3 PrOpened events in rust-lang/rust (same repo, same
        // kind) must produce ONE notification, not three. The
        // body uses the existing pair_label formatter so it reads
        // "rust-lang/rust: 3 PRs opened".
        let _guard = crate::notifications::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::notifications::set_capture_mode(true);
        let _ = crate::notifications::take_captured();

        let mut s = test_state();
        s.config.notifications_enabled = true;
        let e1 = ev("e1", "rust-lang/rust", EventKind::PrOpened, 300);
        let e2 = ev("e2", "rust-lang/rust", EventKind::PrOpened, 200);
        let e3 = ev("e3", "rust-lang/rust", EventKind::PrOpened, 100);
        let _ = update(
            &mut s,
            Message::PolledCycle {
                events: vec![("received", vec![e1, e2, e3])],
                errors: vec![],
            },
        );
        let captured = crate::notifications::take_captured();
        crate::notifications::set_capture_mode(false);
        assert_eq!(
            captured.len(),
            1,
            "3 PRs in same repo must produce one notification, got {captured:?}"
        );
        assert_eq!(captured[0].title, "gh-monitor");
        assert_eq!(
            captured[0].body, "rust-lang/rust: 3 PRs opened",
            "body should use pair_label formatting, got {:?}",
            captured[0].body
        );
    }

    #[test]
    fn notification_aggregates_across_nodes() {
        // Two separate nodes in the same repo with the same kind
        // (e.g. two time windows) must still produce ONE
        // notification with the combined count. The compression
        // algorithm splits nodes on time gaps, so we verify the
        // cross-node aggregation here.
        let _guard = crate::notifications::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::notifications::set_capture_mode(true);
        let _ = crate::notifications::take_captured();

        let mut s = test_state();
        s.config.notifications_enabled = true;
        // 5 PRs from 24h ago (a separate time window) + 2 PRs
        // from 1h ago. Both compress into two nodes for the
        // same repo, and the aggregation must sum them.
        let mut events = Vec::new();
        for i in 0..5 {
            events.push(ev(
                &format!("old-{i}"),
                "rust-lang/rust",
                EventKind::PrOpened,
                86_400,
            ));
        }
        for i in 0..2 {
            events.push(ev(
                &format!("new-{i}"),
                "rust-lang/rust",
                EventKind::PrOpened,
                60,
            ));
        }
        let _ = update(
            &mut s,
            Message::PolledCycle {
                events: vec![("received", events)],
                errors: vec![],
            },
        );
        let captured = crate::notifications::take_captured();
        crate::notifications::set_capture_mode(false);
        assert_eq!(
            captured.len(),
            1,
            "two nodes in same repo/kind must aggregate to one notification, got {captured:?}"
        );
        assert_eq!(captured[0].body, "rust-lang/rust: 7 PRs opened");
    }

    #[test]
    fn notifications_per_repo_are_distinct() {
        // PRs in rust-lang and PRs in tokio-rs are separate
        // (repo, kind) pairs, so they each fire a notification.
        let _guard = crate::notifications::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::notifications::set_capture_mode(true);
        let _ = crate::notifications::take_captured();

        let mut s = test_state();
        s.config.notifications_enabled = true;
        let e1 = ev("e1", "rust-lang/rust", EventKind::PrOpened, 100);
        let e2 = ev("e2", "tokio-rs/tokio", EventKind::PrOpened, 50);
        let _ = update(
            &mut s,
            Message::PolledCycle {
                events: vec![("received", vec![e1, e2])],
                errors: vec![],
            },
        );
        let captured = crate::notifications::take_captured();
        crate::notifications::set_capture_mode(false);
        assert_eq!(captured.len(), 2, "got {captured:?}");
        let bodies: std::collections::BTreeSet<String> =
            captured.iter().map(|n| n.body.clone()).collect();
        assert!(bodies.contains("rust-lang/rust: 1 PR opened"));
        assert!(bodies.contains("tokio-rs/tokio: 1 PR opened"));
    }

    #[test]
    fn toggle_notifications_flips_state() {
        let _guard = crate::notifications::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::notifications::set_capture_mode(false);
        let _ = crate::notifications::take_captured();

        let mut s = test_state();
        assert!(
            !s.config.notifications_enabled,
            "test_state should start with notifications disabled"
        );
        let _ = update(&mut s, Message::ToggleNotifications);
        assert!(
            s.config.notifications_enabled,
            "ToggleNotifications should flip the flag to true"
        );
        let _ = update(&mut s, Message::ToggleNotifications);
        assert!(
            !s.config.notifications_enabled,
            "a second ToggleNotifications should flip it back"
        );
    }
}
