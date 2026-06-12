//! The Iced `Program` implementation. Owns the Iced state and wires
//! the GitHub poller, the timeline state, the canvas, and the
//! passthrough state machine together.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gh_monitor_config::Config;
use gh_monitor_gh::{
    Auth, PollConfig, Poller, PollerHandle, RawEvent,
};
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
    pub prev_snapshot: TimelineSnapshot,
    pub anims: HashMap<NodeId, NodeAnim>,
    pub overlay: OverlayState,
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
    /// Animation frame tick.
    Tick(Instant),
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
    /// Escape pressed — quit.
    Escape,
    /// F5 pressed — refresh now.
    Refresh,
}

/// Run the application. Blocks until the window is closed.
pub fn run(settings: AppSettings) -> anyhow::Result<()> {
    let initial = settings.initial;

    let window = WindowSettings {
        size: Size::new(420.0, 540.0),
        position: window::Position::Default,
        resizable: false,
        closeable: true,
        minimizable: true,
        decorations: false,
        transparent: true,
        level: Level::AlwaysOnTop,
        visible: true,
        ..Default::default()
    };

    // Install the poller before starting the app.
    let _poller_handle = install_poller_if_configured(&initial);

    let result = iced::application(
        move || {
            let state = State {
                config: initial.clone(),
                snapshot: TimelineSnapshot::default(),
                prev_snapshot: TimelineSnapshot::default(),
                anims: HashMap::new(),
                overlay: OverlayState::Idle,
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
        Message::Tick(_now) => Task::none(),
        Message::Polled(events) => {
            state.last_poll_at = Some(Instant::now());
            state.poll_status = PollStatus::Polling;
            apply_events(state, events);
            state.program.snapshot = state.snapshot.clone();
            Task::none()
        }
        Message::PollError(e) => {
            warn!(error = %e, "poll error");
            state.poll_status = PollStatus::Error(e);
            Task::none()
        }
        Message::AuthError(e) => {
            error!(error = %e, "auth error");
            state.poll_status = PollStatus::AuthError(e);
            Task::none()
        }
        Message::HoverEntered => {
            state.overlay = OverlayState::Active;
            let id = state.window_id;
            id.map(window::disable_mouse_passthrough).unwrap_or_else(Task::none)
        }
        Message::HoverLeft => {
            state.overlay = OverlayState::Idle;
            let id = state.window_id;
            id.map(window::enable_mouse_passthrough).unwrap_or_else(Task::none)
        }
        Message::OpenUrl(url) => {
            open_url(&url);
            Task::none()
        }
        Message::WindowResolved(id) => {
            state.window_id = Some(id);
            state.program.window_id = Some(id);
            let passthrough = match state.overlay {
                OverlayState::Idle => window::enable_mouse_passthrough(id),
                OverlayState::Active => window::disable_mouse_passthrough(id),
            };
            passthrough
        }
        Message::Escape => iced::exit(),
        Message::Refresh => Task::none(),
    }
}

fn view(state: &State) -> Element<'_, Message, Theme, iced::Renderer> {
    let canvas = Canvas::new(&state.program)
        .width(iced::Length::Fill)
        .height(iced::Length::Fill);
    let area = iced::widget::MouseArea::new(canvas)
        .on_enter(Message::HoverEntered)
        .on_exit(Message::HoverLeft);
    area.into()
}

fn subscription(_state: &State) -> Subscription<Message> {
    let frames = window::frames().map(Message::Tick);
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
    Subscription::batch([frames, kb, poll, win])
}

fn theme(_state: &State) -> Option<Theme> {
    Some(Theme::Dark)
}

fn title(_state: &State) -> String {
    "gh-monitor".to_string()
}

fn apply_events(state: &mut State, events: Vec<RawEvent>) {
    let groups = group_by_repo(events);
    let compressed = compress(&groups, &CompressionConfig::default());
    let now = chrono::Utc::now();
    let snap = TimelineSnapshot::from_compressed(compressed, now);

    let prev = std::mem::replace(&mut state.snapshot, snap.clone());
    state.prev_snapshot = prev.clone();

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

/// The poll subscription. Bridges a tokio channel into Iced's stream API.
fn poll_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(8, async move |mut output: iced::futures::channel::mpsc::Sender<Message>| {
            let rx = match POLL_RX.lock() {
                Ok(mut g) => g.take(),
                Err(_) => None,
            };
            let Some(mut rx) = rx else {
                warn!("poll subscription: no receiver; exiting");
                return;
            };
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
        })
    })
}

/// Items emitted by the poller into the GUI's stream.
pub enum PollItem {
    Events(Vec<RawEvent>),
    Error(String),
    AuthError(String),
}

use std::sync::Mutex;
static POLL_RX: Mutex<Option<tokio::sync::mpsc::Receiver<PollItem>>> = Mutex::new(None);

/// Install the poller before the app starts. Idempotent.
pub fn install_poller_if_configured(initial: &Config) -> Option<PollerHandle> {
    if initial.pat.trim().is_empty() {
        return None;
    }
    let auth = match Auth::new(initial.pat.clone()) {
        Ok(a) => a,
        Err(_) => return None,
    };
    let poll_cfg = PollConfig {
        username: initial.username.clone(),
        orgs: initial.orgs.clone(),
        repos: initial.repos.clone(),
        interval: Duration::from_secs(initial.poll_interval_secs),
    };
    let poller = match Poller::new(auth, poll_cfg) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "failed to construct poller");
            return None;
        }
    };
    let mut handle = poller.start();

    // Forward raw events to PollItem through a fresh tokio channel.
    let (tx, rx) = tokio::sync::mpsc::channel::<PollItem>(8);
    let mut events = std::mem::replace(&mut handle.events, tokio::sync::mpsc::channel(1).1);
    tokio::spawn(async move {
        while let Some(batch) = events.recv().await {
            if tx.send(PollItem::Events(batch)).await.is_err() {
                break;
            }
        }
    });

    // Stash the receiver for poll_subscription to pick up.
    if let Ok(mut guard) = POLL_RX.lock() {
        *guard = Some(rx);
    }
    Some(handle)
}
