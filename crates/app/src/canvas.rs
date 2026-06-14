//! The Iced `canvas::Program` that renders the timeline.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use iced::border;
use iced::mouse::{self, Cursor, Interaction};
use iced::widget::canvas::{self, Action, Frame, Geometry, Path, Program, Stroke};
use iced::{Color, Event, Point, Rectangle, Size};

use crate::animation::NodeAnim;
use crate::app::Message;
use crate::context_menu::ContextMenu;
use crate::doctor::CheckResult;
use crate::paint::{layout, NodeClass, NodeRect};
use gh_monitor_timeline::{NodeId, TimelineNode, TimelineSnapshot};

/// Glyph-only demo button. The label is part of the chrome row, the
/// icon is the only hit-test target. Width/height in canvas-local
/// units.
const DEMO_BTN_WIDTH: f32 = 24.0;
const DEMO_BTN_HEIGHT: f32 = 24.0;
/// Margin from the canvas edges to the demo button and indicator.
const DEMO_CHROME_MARGIN: f32 = 8.0;
/// Gap between the demo button and the indicator rectangle when
/// both are drawn side by side.
const DEMO_BTN_INDICATOR_GAP: f32 = 8.0;
/// Height of the "Demo running — XXs left" indicator. Same as the
/// button so they share a top row.
const DEMO_INDICATOR_HEIGHT: f32 = 24.0;

/// Width of the bell-icon hit-test rect. Sits to the LEFT of the
/// demo button in the same top row.
pub(crate) const BELL_HIT_WIDTH: f32 = 28.0;
/// Height of the bell-icon hit-test rect.
pub(crate) const BELL_HIT_HEIGHT: f32 = 24.0;
/// Gap between the bell and the demo button.
pub(crate) const BELL_BTN_GAP: f32 = 8.0;

/// The canvas program. Holds the current snapshot, the per-node animation
/// state, and the current window id.
#[derive(Debug)]
pub struct TimelineProgram {
    pub snapshot: TimelineSnapshot,
    pub anims: HashMap<NodeId, NodeAnim>,
    pub window_id: Option<iced::window::Id>,
    /// Optional status banner rendered in the centre of the canvas.
    pub status: Option<String>,
    /// Whether the user has no PAT configured. The status banner will
    /// include setup instructions in that case.
    pub needs_setup: bool,
    /// Seconds left in the active demo, or `None` when no demo is
    /// running. The "▶" demo button is drawn unconditionally; the
    /// indicator is only drawn when this is `Some`. Set from the
    /// app's `sync_program` after every state change.
    pub demo_remaining_secs: Option<u64>,
    /// Whether desktop notifications are enabled. The bell icon
    /// in the top-right of the canvas reflects this: 🔔 when
    /// enabled, 🔕 when muted. Set from the app's
    /// `sync_program` after every state change.
    pub notifications_enabled: bool,
    /// The right-click context menu, if open. When `Some` the
    /// canvas renders the menu in place of the timeline. Set from
    /// the app's `sync_program` after every state change.
    pub context_menu: Option<ContextMenu>,
    /// `true` when the in-app Doctor diagnostics page is showing in
    /// place of the timeline. Set from the app's `sync_program`
    /// after every state change.
    pub show_doctor: bool,
    /// `true` when the About page is showing in place of the
    /// timeline. Set from the app's `sync_program` after every
    /// state change.
    pub show_about: bool,
    /// Doctor check results. Empty until the async check has run.
    /// Set from the app's `sync_program` after every state change.
    pub doctor_results: Vec<CheckResult>,
    /// `true` while a doctor check is in flight. Drawn as a
    /// "checking…" hint on the doctor page. Set from the app's
    /// `sync_program` after every state change.
    pub doctor_running: bool,
}

impl Default for TimelineProgram {
    fn default() -> Self {
        Self::new()
    }
}

impl TimelineProgram {
    pub fn new() -> Self {
        Self {
            snapshot: TimelineSnapshot::default(),
            anims: HashMap::new(),
            window_id: None,
            status: None,
            needs_setup: false,
            demo_remaining_secs: None,
            notifications_enabled: false,
            context_menu: None,
            show_doctor: false,
            show_about: false,
            doctor_results: Vec::new(),
            doctor_running: false,
        }
    }

    pub fn update_snapshot(&mut self, snap: TimelineSnapshot) {
        self.snapshot = snap;
    }

    /// Replace the per-node animation map. Called after a poll diff so the
    /// canvas can drive fade-in and pulse animations from the same state
    /// the app's `State` uses.
    pub fn set_anims(&mut self, anims: HashMap<NodeId, NodeAnim>) {
        self.anims = anims;
    }

    pub fn last_pressed_url(&self) -> Option<String> {
        // Placeholder: the canvas's Program::update publishes the URL
        // directly. This method exists for the view's safety-net
        // on_press fallback; it never fires in practice because the
        // canvas's own hit-test publishes first.
        None
    }
}

/// The rectangle of the bell icon. Sits in the chrome strip to
/// the LEFT of the demo button so a click on the bell never falls
/// through to the demo button or a node.
pub(crate) fn bell_rect(bounds: Rectangle) -> Rectangle {
    let demo_x = bounds.width - DEMO_CHROME_MARGIN - DEMO_BTN_WIDTH;
    Rectangle {
        x: demo_x - BELL_BTN_GAP - BELL_HIT_WIDTH,
        y: DEMO_CHROME_MARGIN,
        width: BELL_HIT_WIDTH,
        height: BELL_HIT_HEIGHT,
    }
}

/// Hit-test the bell icon. Returns `true` when `pos` lies within
/// the icon's clickable rect. `pos` is the cursor position
/// *relative to* `bounds` (i.e. `cursor.position_in(bounds)`).
pub(crate) fn bell_hit(bounds: Rectangle, pos: Point) -> bool {
    bell_rect(bounds).contains(pos)
}

impl Program<Message, iced::Theme, iced::Renderer> for TimelineProgram {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        draw_background(&mut frame, bounds);

        // Modal pages take over the whole canvas. Render whichever
        // one is active and return — the timeline chrome (bell,
        // demo button, status banner) is hidden so the user can
        // see the page content without the chrome overlapping it.
        if self.show_about {
            draw_about_page(&mut frame, bounds);
            return vec![frame.into_geometry()];
        }
        if self.show_doctor {
            draw_doctor_page(
                &mut frame,
                bounds,
                &self.doctor_results,
                self.doctor_running,
            );
            return vec![frame.into_geometry()];
        }
        if let Some(menu) = &self.context_menu {
            let hovered = menu.selected;
            // Draw the timeline underneath so the user still has
            // visual context (the menu is a popup, not a page).
            let has_status_banner = self.status.is_some() && !self.snapshot.nodes.is_empty();
            let (rects, _size) = layout(&self.snapshot, bounds.width, has_status_banner);
            let cursor_pos = cursor.position_in(bounds);
            let bell_hovered = cursor_pos.map(|p| bell_hit(bounds, p)).unwrap_or(false);
            draw_bell(&mut frame, bounds, self.notifications_enabled, bell_hovered);
            for (i, node) in self.snapshot.nodes.iter().enumerate() {
                if i >= rects.len() {
                    break;
                }
                let rect = rects[i];
                let anim = self
                    .anims
                    .get(&node.id)
                    .cloned()
                    .unwrap_or_else(|| NodeAnim::new_insert(Instant::now()));
                let now = Instant::now();
                let opacity = anim.opacity_at(now);
                let pulse = anim.pulse_at(now);
                draw_node(
                    &mut frame,
                    node,
                    rect,
                    opacity,
                    pulse,
                    false,
                    NodeClass::from_node_kind(node.kind),
                );
            }
            if let Some(s) = &self.status {
                draw_status_banner(&mut frame, bounds, s);
            }
            draw_context_menu(&mut frame, bounds, menu, hovered);
            draw_demo_button(&mut frame, bounds);
            if let Some(remaining) = self.demo_remaining_secs {
                draw_demo_indicator(&mut frame, bounds, remaining);
            }
            return vec![frame.into_geometry()];
        }

        let demo_active = self.demo_remaining_secs.is_some();
        let has_status_banner =
            self.status.is_some() && !self.snapshot.nodes.is_empty() && !demo_active;
        let (rects, _size) = layout(&self.snapshot, bounds.width, has_status_banner);
        let cursor_pos = cursor.position_in(bounds);

        // Bell icon: drawn before the nodes so the nodes sit on
        // top visually, but the hit-test region is its own
        // dedicated rect in the chrome strip — separate from the
        // first node's time-label area and from the demo button,
        // so a click on the bell is unambiguous.
        let bell_hovered = cursor_pos.map(|p| bell_hit(bounds, p)).unwrap_or(false);
        draw_bell(&mut frame, bounds, self.notifications_enabled, bell_hovered);

        for (i, node) in self.snapshot.nodes.iter().enumerate() {
            if i >= rects.len() {
                break;
            }
            let rect = rects[i];
            let anim = self
                .anims
                .get(&node.id)
                .cloned()
                .unwrap_or_else(|| NodeAnim::new_insert(Instant::now()));
            let now = Instant::now();
            let opacity = anim.opacity_at(now);
            let pulse = anim.pulse_at(now);
            let hovering = if let Some(p) = cursor_pos {
                rect.contains(p)
            } else {
                false
            };
            draw_node(
                &mut frame,
                node,
                rect,
                opacity,
                pulse,
                hovering,
                NodeClass::from_node_kind(node.kind),
            );
        }

        // Status banner overlay (drawn on top of the timeline).
        // When a demo is active we suppress the empty/setup state so
        // the user sees the demo timeline instead of "No personal
        // access token set." or "No recent activity" during the
        // first ~1s gap before the first scripted event fires.
        if self.snapshot.nodes.is_empty() && !demo_active {
            draw_empty_state(&mut frame, bounds, self.status.as_deref(), self.needs_setup);
        } else if let Some(s) = &self.status {
            draw_status_banner(&mut frame, bounds, s);
        }

        // Demo chrome: the "▶" demo button is always drawn in the
        // top-right; the "Demo running — XXs left" indicator is
        // drawn alongside it only when a demo is active. Drawn
        // last so it overlays any status banner.
        draw_demo_button(&mut frame, bounds);
        if let Some(remaining) = self.demo_remaining_secs {
            draw_demo_indicator(&mut frame, bounds, remaining);
        }

        vec![frame.into_geometry()]
    }

    fn update(
        &self,
        _state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<Message>> {
        let cursor_pos = cursor.position_in(bounds);

        // Context menu takes priority over timeline interactions.
        // While the menu is open, the menu's own hit-test owns
        // mouse events: a left-click inside the menu picks the
        // item, a click outside dismisses, and cursor movement
        // updates the hover highlight. Without this gate the
        // timeline's node/drag hit-test would fire under the menu
        // and steal clicks.
        if let Some(menu) = &self.context_menu {
            match event {
                Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                    if let Some(p) = cursor_pos {
                        return Some(Action::publish(Message::ContextMenuHover(
                            menu.item_at(p, bounds),
                        )));
                    }
                }
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                    if let Some(p) = cursor_pos {
                        if menu.contains(p, bounds) {
                            if let Some(idx) = menu.item_at(p, bounds) {
                                return Some(Action::publish(Message::ContextMenuItem(
                                    menu.items[idx],
                                )));
                            }
                        } else {
                            return Some(Action::publish(Message::DismissContextMenu));
                        }
                    }
                }
                _ => {}
            }
            // The menu is open but the event is something the menu
            // doesn't care about (e.g. a right-click or a keyboard
            // event). Fall through to the redraw-request path below
            // by returning None at the end of the function.
        }

        let demo_active = self.demo_remaining_secs.is_some();
        let has_status_banner =
            self.status.is_some() && !self.snapshot.nodes.is_empty() && !demo_active;
        let (rects, _) = layout(&self.snapshot, bounds.width, has_status_banner);

        if let Some(p) = cursor_pos {
            // Right-click on the canvas opens the context menu
            // regardless of where on the canvas the click lands.
            // The context menu's own update path handles clicks
            // INSIDE the menu separately; this is the path for
            // opening the menu in the first place.
            if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) = event {
                return Some(Action::publish(Message::OpenContextMenu(p)));
            }
            if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event {
                // Bell first (leftmost in the chrome strip), then
                // demo button, then nodes, then drag. Both chrome
                // buttons must take priority over any node that
                // might happen to overlap (the status banner and
                // the demo button can both be over node #1).
                if bell_hit(bounds, p) {
                    return Some(Action::publish(Message::ToggleNotifications));
                }
                if demo_button_rect(bounds).contains(p) {
                    return Some(Action::publish(Message::StartDemo));
                }
                for rect in &rects {
                    if rect.contains(p) {
                        let node = &self.snapshot.nodes[rect.index];
                        // Open the URL on press, but DON'T capture the
                        // event — we want subsequent widgets (and the
                        // outer MouseArea) to also see the press so that
                        // window-drag and click-to-open both work
                        // reliably. The OS will short-circuit a drag
                        // on a quick click, and the URL still opens.
                        return Some(Action::publish(Message::OpenUrl(node.target_url.clone())));
                    }
                }
                // Empty area: trigger window drag. Without this,
                // click-and-drag only worked the first time because
                // the outer MouseArea's `on_press` was getting
                // shadowed by the canvas's `and_capture` action
                // once the user's first drag ended on a node.
                return Some(Action::publish(Message::DragWindow));
            }
        }
        // Self-driven redraw: when any node is mid-animation
        // (fade-in or pulse), request a redraw ~30×/sec so the
        // canvas keeps animating. When nothing is animating,
        // return `None` and let Iced skip the next frame — the
        // canvas only repaints on state changes or user input.
        let now = Instant::now();
        let any_active = self
            .anims
            .values()
            .any(|a| a.opacity.is_animating(now) || a.pulse.is_animating(now));
        if any_active {
            Some(Action::request_redraw_at(now + Duration::from_millis(33)))
        } else {
            None
        }
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        let demo_active = self.demo_remaining_secs.is_some();
        let has_status_banner =
            self.status.is_some() && !self.snapshot.nodes.is_empty() && !demo_active;
        let (rects, _) = layout(&self.snapshot, bounds.width, has_status_banner);
        if let Some(p) = cursor.position_in(bounds) {
            // Bell + demo button take priority over nodes — both
            // live in the chrome strip that overlaps the first
            // node, so a node hit-test alone would show a pointer
            // cursor even when the cursor is on the button.
            if bell_hit(bounds, p) {
                return Interaction::Pointer;
            }
            if demo_button_rect(bounds).contains(p) {
                return Interaction::Pointer;
            }
            for rect in &rects {
                if rect.contains(p) {
                    return Interaction::Pointer;
                }
            }
        }
        Interaction::default()
    }
}

fn draw_background(frame: &mut Frame, bounds: Rectangle) {
    let bg = Path::rounded_rectangle(
        Point::new(0.0, 0.0),
        Size::new(bounds.width, bounds.height),
        border::Radius::new(12.0),
    );
    frame.fill(
        &bg,
        Color {
            r: 0.07,
            g: 0.07,
            b: 0.08,
            a: 0.78,
        },
    );
    frame.stroke(
        &bg,
        Stroke::default()
            .with_color(Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.06,
            })
            .with_width(1.0),
    );
}

/// Decide which lines to display in the empty state, based on whether
/// the user still needs to configure a token and on any active status.
pub(crate) fn empty_state_lines(needs_setup: bool, status: Option<&str>) -> Vec<String> {
    if needs_setup {
        vec![
            "gh-monitor".to_string(),
            String::new(),
            "No personal access token set.".to_string(),
            "Run one of:".to_string(),
            "  gh-monitor config edit".to_string(),
            "  GH_MONITOR_PAT=ghp_... gh-monitor".to_string(),
        ]
    } else if let Some(s) = status {
        vec![s.to_string()]
    } else {
        vec!["No recent activity".to_string()]
    }
}

/// Vertical spacing between lines in the empty state. The text size
/// is 13pt so 20px gives a comfortable line height.
pub(crate) const EMPTY_STATE_LINE_HEIGHT: f32 = 20.0;

/// Decide what text to show in the status banner. Currently an identity
/// function; exists so we can add truncation or other transforms later
/// without touching the draw code.
pub(crate) fn status_banner_text(text: &str) -> &str {
    text
}

/// The rectangle of the "🎬 Demo" button in canvas-local coordinates.
/// Pure helper so the hit-test and the draw path agree on the
/// exact bounds.
fn demo_button_rect(bounds: Rectangle) -> Rectangle {
    Rectangle {
        x: bounds.width - DEMO_CHROME_MARGIN - DEMO_BTN_WIDTH,
        y: DEMO_CHROME_MARGIN,
        width: DEMO_BTN_WIDTH,
        height: DEMO_BTN_HEIGHT,
    }
}

/// The rectangle of the "Demo running — XXs left" indicator.
/// Sits to the LEFT of the demo button in the same top row.
fn demo_indicator_rect(bounds: Rectangle) -> Rectangle {
    let x_right_of_indicator =
        bounds.width - DEMO_CHROME_MARGIN - DEMO_BTN_WIDTH - DEMO_BTN_INDICATOR_GAP;
    Rectangle {
        x: DEMO_CHROME_MARGIN,
        y: DEMO_CHROME_MARGIN,
        width: (x_right_of_indicator - DEMO_CHROME_MARGIN).max(0.0),
        height: DEMO_INDICATOR_HEIGHT,
    }
}

/// Draw the bell icon in the top-right of the canvas. `enabled`
/// picks the indicator (filled gold circle when notifications
/// are on, outlined gray circle when off) and `hovered` adds a
/// subtle background pad so the user can see the click target.
/// Sits to the LEFT of the demo button in the same top row.
/// The indicator is a `Path::circle` so it never depends on an
/// emoji font being installed.
fn draw_bell(frame: &mut Frame, bounds: Rectangle, enabled: bool, hovered: bool) {
    let rect = bell_rect(bounds);
    if hovered {
        let pad = Path::rounded_rectangle(
            Point::new(rect.x - 2.0, rect.y - 2.0),
            Size::new(rect.width + 4.0, rect.height + 4.0),
            border::Radius::new(6.0),
        );
        frame.fill(
            &pad,
            Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.08,
            },
        );
    }
    let center = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    let dot = Path::circle(center, 8.0);
    if enabled {
        frame.fill(
            &dot,
            Color {
                r: 1.0,
                g: 0.90,
                b: 0.55,
                a: 0.95,
            },
        );
    } else {
        frame.stroke(
            &dot,
            Stroke::default()
                .with_color(Color {
                    r: 0.65,
                    g: 0.65,
                    b: 0.70,
                    a: 0.85,
                })
                .with_width(1.5),
        );
    }
}

/// Draw the "▶" demo button in the top-right corner. The button is
/// always visible (even when no demo is running) so the user knows
/// the feature exists. Glyph-only so it doesn't overlap the first
/// node's time label.
fn draw_demo_button(frame: &mut Frame, bounds: Rectangle) {
    let rect = demo_button_rect(bounds);
    let bg = Path::rounded_rectangle(
        Point::new(rect.x, rect.y),
        Size::new(rect.width, rect.height),
        border::Radius::new(6.0),
    );
    frame.fill(
        &bg,
        Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 0.08,
        },
    );
    frame.stroke(
        &bg,
        Stroke::default()
            .with_color(Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.18,
            })
            .with_width(1.0),
    );
    // Center the "▶" glyph in the 24x24 button.
    frame.fill_text(canvas::Text {
        content: "\u{25B6}".to_string(),
        position: Point::new(rect.x + 7.0, rect.y + 4.0),
        max_width: rect.width,
        color: Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 0.95,
        },
        size: 12.0.into(),
        ..canvas::Text::default()
    });
}

/// Draw the "Demo running — XXs left" indicator. A short pill
/// with the same background as the button and a "Demo running"
/// label, so the user knows the timer is counting down.
fn draw_demo_indicator(frame: &mut Frame, bounds: Rectangle, remaining_secs: u64) {
    let rect = demo_indicator_rect(bounds);
    if rect.width <= 0.0 {
        return;
    }
    let bg = Path::rounded_rectangle(
        Point::new(rect.x, rect.y),
        Size::new(rect.width, rect.height),
        border::Radius::new(6.0),
    );
    frame.fill(
        &bg,
        Color {
            r: 0.30,
            g: 0.45,
            b: 0.80,
            a: 0.40,
        },
    );
    frame.fill_text(canvas::Text {
        content: format!("Demo running — {remaining_secs}s left"),
        position: Point::new(rect.x + 10.0, rect.y + 5.0),
        max_width: rect.width - 14.0,
        color: Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 0.95,
        },
        size: 12.0.into(),
        ..canvas::Text::default()
    });
}

/// Draw the "empty" state in the middle of the canvas. Shown when the
/// timeline has no nodes to display.
fn draw_empty_state(frame: &mut Frame, bounds: Rectangle, status: Option<&str>, needs_setup: bool) {
    let lines = empty_state_lines(needs_setup, status);
    let line_height = EMPTY_STATE_LINE_HEIGHT;
    let mut y = bounds.height / 2.0 - (lines.len() as f32 * line_height) / 2.0;
    for line in lines {
        frame.fill_text(canvas::Text {
            content: line,
            position: Point::new(20.0, y),
            max_width: bounds.width - 40.0,
            color: Color {
                r: 0.85,
                g: 0.85,
                b: 0.9,
                a: 0.92,
            },
            size: 13.0.into(),
            ..canvas::Text::default()
        });
        y += line_height;
    }
}

/// Draw a transient status banner at the top of the canvas (e.g. an
/// error message).
fn draw_status_banner(frame: &mut Frame, bounds: Rectangle, text: &str) {
    let banner_height = 32.0;
    let bg = Path::rounded_rectangle(
        Point::new(8.0, 4.0),
        Size::new(bounds.width - 16.0, banner_height),
        border::Radius::new(6.0),
    );
    frame.fill(
        &bg,
        Color {
            r: 0.7,
            g: 0.2,
            b: 0.2,
            a: 0.45,
        },
    );
    let banner_text = status_banner_text(text);
    frame.fill_text(canvas::Text {
        content: banner_text.to_string(),
        position: Point::new(16.0, 14.0),
        max_width: bounds.width - 32.0,
        color: Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 0.95,
        },
        size: 12.0.into(),
        ..canvas::Text::default()
    });
}

/// Background colour for a node, given its class and current opacity.
/// Pure helper so the colour choices can be unit-tested without a
/// renderer.
pub(crate) fn node_bg_color(class: NodeClass, opacity: f32) -> Color {
    match class {
        NodeClass::Group => Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 0.04 * opacity,
        },
        NodeClass::Standalone => Color {
            r: 1.0,
            g: 0.85,
            b: 0.30,
            a: 0.18 * opacity,
        },
    }
}

/// Accent-dot colour for a standalone node. White at high alpha so
/// the dot pops against the gold-tinted background. Pure helper so
/// the contrast with `node_bg_color` can be asserted in tests.
pub(crate) fn standalone_dot_color(opacity: f32) -> Color {
    Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: opacity * 0.9,
    }
}

/// Vertical spacing between pair-label rows in a node. 12pt text
/// needs more than 14px to breathe.
pub(crate) const PAIR_LABEL_LINE_HEIGHT: f32 = 16.0;

/// The maximum width allotted to the time label, clamped to the
/// node's width so a narrow node never bleeds the label past its
/// left edge.
pub(crate) fn time_label_max_width(rect: NodeRect) -> f32 {
    (rect.width - 24.0).max(0.0)
}

fn draw_node(
    frame: &mut Frame,
    node: &TimelineNode,
    rect: NodeRect,
    opacity: f32,
    pulse: f32,
    hovering: bool,
    class: NodeClass,
) {
    let bg = Path::rounded_rectangle(
        Point::new(rect.x, rect.y),
        Size::new(rect.width, rect.height),
        border::Radius::new(8.0),
    );
    frame.fill(&bg, node_bg_color(class, opacity));

    if pulse > 0.01 {
        let pad = 2.0 + pulse * 4.0;
        let halo = Path::rounded_rectangle(
            Point::new(rect.x - pad, rect.y - pad),
            Size::new(rect.width + 2.0 * pad, rect.height + 2.0 * pad),
            border::Radius::new(8.0 + pad),
        );
        frame.fill(
            &halo,
            Color {
                r: 0.40,
                g: 0.85,
                b: 1.0,
                a: 0.18 * pulse,
            },
        );
    }

    if hovering {
        let pad = 1.0;
        let ring = Path::rounded_rectangle(
            Point::new(rect.x - pad, rect.y - pad),
            Size::new(rect.width + 2.0 * pad, rect.height + 2.0 * pad),
            border::Radius::new(9.0),
        );
        frame.stroke(
            &ring,
            Stroke::default()
                .with_color(Color {
                    r: 0.55,
                    g: 0.85,
                    b: 1.0,
                    a: 0.7,
                })
                .with_width(1.5),
        );
    }

    // Repo name (top-left).
    frame.fill_text(canvas::Text {
        content: node.repo.clone(),
        position: Point::new(rect.x + 12.0, rect.y + 8.0),
        max_width: rect.width - 24.0,
        color: Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: opacity,
        },
        size: 14.0.into(),
        ..canvas::Text::default()
    });

    // Pair labels (one per (kind,count) pair).
    let mut y = rect.y + 28.0;
    for (pair,) in &node.pairs {
        let s = crate::paint::pair_label(pair.kind, pair.count);
        frame.fill_text(canvas::Text {
            content: s,
            position: Point::new(rect.x + 12.0, y),
            max_width: rect.width - 24.0,
            color: Color {
                r: 0.85,
                g: 0.85,
                b: 0.9,
                a: opacity * 0.9,
            },
            size: 12.0.into(),
            ..canvas::Text::default()
        });
        y += PAIR_LABEL_LINE_HEIGHT;
    }

    // Time label (top-right). Clamp the max-width to the node's
    // width so a narrow node never bleeds the label past its left
    // edge (which would overlap the repo name).
    frame.fill_text(canvas::Text {
        content: node.time_label.clone(),
        position: Point::new(rect.x + rect.width - 12.0, rect.y + 8.0),
        max_width: time_label_max_width(rect),
        color: Color {
            r: 0.6,
            g: 0.6,
            b: 0.65,
            a: opacity * 0.8,
        },
        size: 12.0.into(),
        align_x: iced::alignment::Horizontal::Right.into(),
        ..canvas::Text::default()
    });

    // Standalone accent dot. White at high alpha so it pops
    // against the gold-tinted background.
    if matches!(class, NodeClass::Standalone) {
        let dot = Path::circle(Point::new(rect.x + rect.width - 14.0, rect.y + 26.0), 3.0);
        frame.fill(&dot, standalone_dot_color(opacity));
    }
}

/// Draw the right-click context menu in place of the timeline. The
/// menu is opaque (it has its own background) and floats over the
/// timeline so the user keeps visual context. `hovered` highlights
/// the item under the cursor; pass `None` for no highlight.
fn draw_context_menu(
    frame: &mut Frame,
    bounds: Rectangle,
    menu: &ContextMenu,
    hovered: Option<usize>,
) {
    let menu_rect = menu.rect(bounds);
    // Background.
    let bg = Path::rounded_rectangle(
        Point::new(menu_rect.x, menu_rect.y),
        Size::new(menu_rect.width, menu_rect.height),
        border::Radius::new(6.0),
    );
    frame.fill(
        &bg,
        Color {
            r: 0.10,
            g: 0.10,
            b: 0.12,
            a: 0.96,
        },
    );
    frame.stroke(
        &bg,
        Stroke::default()
            .with_color(Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.18,
            })
            .with_width(1.0),
    );

    // Walk the items in display order, drawing each row at its
    // rect. Separators get a thin horizontal rule instead of a
    // label.
    for (i, item) in menu.items.iter().enumerate() {
        if item.is_separator() {
            // The separator's band is the gap between the previous
            // and next item; we draw a 1px line vertically
            // centred in that band.
            if let Some(prev_rect) = (i > 0).then(|| menu.item_rect(i - 1, bounds)).flatten() {
                if let Some(next_rect) = menu.item_rect(i + 1, bounds) {
                    let y = (prev_rect.y + prev_rect.height + next_rect.y) / 2.0;
                    let line = Path::line(
                        Point::new(menu_rect.x + 8.0, y),
                        Point::new(menu_rect.x + menu_rect.width - 8.0, y),
                    );
                    frame.stroke(
                        &line,
                        Stroke::default()
                            .with_color(Color {
                                r: 1.0,
                                g: 1.0,
                                b: 1.0,
                                a: 0.12,
                            })
                            .with_width(1.0),
                    );
                }
            }
            continue;
        }
        let Some(item_rect) = menu.item_rect(i, bounds) else {
            continue;
        };
        // Hover highlight.
        if hovered == Some(i) {
            let pad = 2.0;
            let hl = Path::rounded_rectangle(
                Point::new(item_rect.x, item_rect.y + pad),
                Size::new(item_rect.width, item_rect.height - 2.0 * pad),
                border::Radius::new(4.0),
            );
            frame.fill(
                &hl,
                Color {
                    r: 0.30,
                    g: 0.45,
                    b: 0.80,
                    a: 0.55,
                },
            );
        }
        frame.fill_text(canvas::Text {
            content: item.label().to_string(),
            position: Point::new(item_rect.x + 12.0, item_rect.y + 6.0),
            max_width: item_rect.width - 24.0,
            color: Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.95,
            },
            size: 13.0.into(),
            ..canvas::Text::default()
        });
    }
}

/// Draw the in-app Doctor diagnostics page. Renders a title, a
/// "checking…" hint while the async run is in flight, and one row
/// per check result: status bullet (green/yellow/red), label, and
/// detail. Pure data: the page does not need to interact with the
/// timeline.
fn draw_doctor_page(frame: &mut Frame, bounds: Rectangle, results: &[CheckResult], running: bool) {
    // Title.
    frame.fill_text(canvas::Text {
        content: "Doctor".to_string(),
        position: Point::new(20.0, 18.0),
        max_width: bounds.width - 40.0,
        color: Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 0.98,
        },
        size: 18.0.into(),
        ..canvas::Text::default()
    });
    // Subtitle.
    let subtitle = if running && results.is_empty() {
        "checking…"
    } else {
        "press Escape to return"
    };
    frame.fill_text(canvas::Text {
        content: subtitle.to_string(),
        position: Point::new(20.0, 42.0),
        max_width: bounds.width - 40.0,
        color: Color {
            r: 0.70,
            g: 0.70,
            b: 0.75,
            a: 0.85,
        },
        size: 12.0.into(),
        ..canvas::Text::default()
    });

    // One row per check, starting at y=70. Bullet on the left,
    // label + detail to the right.
    let mut y = 70.0;
    for r in results {
        let bullet_color = match r.status {
            crate::doctor::Status::Ok => Color {
                r: 0.40,
                g: 0.85,
                b: 0.45,
                a: 0.95,
            },
            crate::doctor::Status::Warn => Color {
                r: 0.95,
                g: 0.75,
                b: 0.30,
                a: 0.95,
            },
            crate::doctor::Status::Fail => Color {
                r: 0.95,
                g: 0.35,
                b: 0.35,
                a: 0.95,
            },
        };
        let bullet = Path::circle(Point::new(28.0, y + 8.0), 4.0);
        frame.fill(&bullet, bullet_color);
        frame.fill_text(canvas::Text {
            content: format!("{}: {}", r.label, r.detail),
            position: Point::new(40.0, y),
            max_width: bounds.width - 56.0,
            color: Color {
                r: 0.92,
                g: 0.92,
                b: 0.95,
                a: 0.95,
            },
            size: 12.0.into(),
            ..canvas::Text::default()
        });
        y += 22.0;
    }
}

/// Draw the About page: version, repo URL, and license. Pure
/// data — no interaction with the timeline.
fn draw_about_page(frame: &mut Frame, bounds: Rectangle) {
    let title_color = Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 0.98,
    };
    let body_color = Color {
        r: 0.85,
        g: 0.85,
        b: 0.9,
        a: 0.92,
    };
    let mut y = 24.0;
    // Title.
    frame.fill_text(canvas::Text {
        content: "About".to_string(),
        position: Point::new(20.0, y),
        max_width: bounds.width - 40.0,
        color: title_color,
        size: 18.0.into(),
        ..canvas::Text::default()
    });
    y += 32.0;
    // Version.
    frame.fill_text(canvas::Text {
        content: format!("gh-monitor v{}", env!("CARGO_PKG_VERSION")),
        position: Point::new(20.0, y),
        max_width: bounds.width - 40.0,
        color: body_color,
        size: 13.0.into(),
        ..canvas::Text::default()
    });
    y += 22.0;
    // Repo URL + license.
    frame.fill_text(canvas::Text {
        content: "https://github.com/clankercode/gh-monitor1".to_string(),
        position: Point::new(20.0, y),
        max_width: bounds.width - 40.0,
        color: body_color,
        size: 12.0.into(),
        ..canvas::Text::default()
    });
    y += 18.0;
    frame.fill_text(canvas::Text {
        content: "Licensed under MIT OR Apache-2.0".to_string(),
        position: Point::new(20.0, y),
        max_width: bounds.width - 40.0,
        color: body_color,
        size: 12.0.into(),
        ..canvas::Text::default()
    });
    y += 24.0;
    // Return hint.
    frame.fill_text(canvas::Text {
        content: "press Escape to return".to_string(),
        position: Point::new(20.0, y),
        max_width: bounds.width - 40.0,
        color: Color {
            r: 0.70,
            g: 0.70,
            b: 0.75,
            a: 0.85,
        },
        size: 11.0.into(),
        ..canvas::Text::default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_menu::MenuItem;

    #[test]
    fn empty_state_lines_setup_when_needs_setup() {
        let lines = empty_state_lines(true, None);
        assert_eq!(
            lines,
            vec![
                "gh-monitor".to_string(),
                String::new(),
                "No personal access token set.".to_string(),
                "Run one of:".to_string(),
                "  gh-monitor config edit".to_string(),
                "  GH_MONITOR_PAT=ghp_... gh-monitor".to_string(),
            ]
        );
    }

    #[test]
    fn empty_state_lines_status_when_status_set() {
        let lines = empty_state_lines(false, Some("auth failed"));
        assert_eq!(lines, vec!["auth failed".to_string()]);
    }

    #[test]
    fn empty_state_lines_default_when_no_status_no_setup() {
        let lines = empty_state_lines(false, None);
        assert_eq!(lines, vec!["No recent activity".to_string()]);
    }

    #[test]
    fn demo_button_anchored_to_top_right_with_margin() {
        // The button must always sit in the top-right of the
        // canvas with the configured margin so a click there
        // hits `Message::StartDemo` instead of a node.
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 420.0,
            height: 540.0,
        };
        let r = demo_button_rect(bounds);
        assert_eq!(r.x, 420.0 - 8.0 - 24.0);
        assert_eq!(r.y, 8.0);
        assert_eq!(r.width, 24.0);
        assert_eq!(r.height, 24.0);
        // The centre of the button is inside the button.
        assert!(r.contains(Point::new(r.x + r.width / 2.0, r.y + r.height / 2.0)));
        // The left edge of the first node (x=12) is NOT in the
        // button.
        assert!(!r.contains(Point::new(12.0, r.y + 4.0)));
    }

    #[test]
    fn demo_indicator_shares_top_row_with_button() {
        // The indicator must not overlap the button and must be
        // drawn at the same y as the button.
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 420.0,
            height: 540.0,
        };
        let btn = demo_button_rect(bounds);
        let ind = demo_indicator_rect(bounds);
        assert_eq!(ind.y, btn.y, "indicator and button share top row");
        assert_eq!(ind.height, btn.height);
        assert!(
            ind.x + ind.width <= btn.x,
            "indicator must end before the button starts: ind.x={} ind.width={} btn.x={}",
            ind.x,
            ind.width,
            btn.x
        );
    }

    // ---- bell icon tests ----

    fn test_bounds() -> Rectangle {
        Rectangle {
            x: 0.0,
            y: 0.0,
            width: 420.0,
            height: 540.0,
        }
    }

    fn cursor_at(p: Point) -> Cursor {
        Cursor::Available(p)
    }

    #[test]
    fn bell_anchored_to_left_of_demo_button() {
        // The bell must sit in the same top row as the demo
        // button, to its LEFT, with the configured gap.
        let bounds = test_bounds();
        let btn = demo_button_rect(bounds);
        let bell = bell_rect(bounds);
        assert_eq!(bell.y, btn.y, "bell and demo button share the top row");
        assert_eq!(bell.height, btn.height);
        assert!(
            bell.x + bell.width + BELL_BTN_GAP <= btn.x + 0.01,
            "bell must end `BELL_BTN_GAP` before demo button starts: \
             bell.x={} bell.width={} gap={} btn.x={}",
            bell.x,
            bell.width,
            BELL_BTN_GAP,
            btn.x
        );
    }

    #[test]
    fn bell_hit_inside_rect() {
        let bounds = test_bounds();
        let r = bell_rect(bounds);
        let cx = r.x + r.width / 2.0;
        let cy = r.y + r.height / 2.0;
        assert!(bell_hit(bounds, Point::new(cx, cy)));
    }

    #[test]
    fn bell_hit_outside_rect() {
        let bounds = test_bounds();
        // Far left of the canvas is not the bell.
        assert!(!bell_hit(bounds, Point::new(5.0, 5.0)));
        // The demo-button area is to the RIGHT of the bell —
        // a click there is not the bell.
        let btn = demo_button_rect(bounds);
        let cx = btn.x + btn.width / 2.0;
        let cy = btn.y + btn.height / 2.0;
        assert!(
            !bell_hit(bounds, Point::new(cx, cy)),
            "demo button area must not hit the bell"
        );
    }

    // ---- smart redraw tests ----

    fn make_program_with_anim(anim: NodeAnim) -> TimelineProgram {
        let mut p = TimelineProgram::new();
        let id = NodeId::new("test");
        p.snapshot.nodes.push(TimelineNode {
            id: id.clone(),
            kind: gh_monitor_timeline::NodeKind::Group,
            repo: "x/y".to_string(),
            pairs: vec![],
            time_label: "1 hr ago".to_string(),
            earliest: chrono::Utc::now(),
            latest: chrono::Utc::now(),
            target_url: "https://github.com/x/y".to_string(),
        });
        p.anims.insert(id, anim);
        p
    }

    fn press_event() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
    }

    #[test]
    fn update_requests_redraw_when_animation_active() {
        // A freshly-inserted node has an opacity animation that
        // is in progress for ~400ms. update() with a no-op event
        // must therefore request a redraw so the canvas keeps
        // animating.
        let p = make_program_with_anim(NodeAnim::new_insert(Instant::now()));
        let bounds = test_bounds();
        let cursor = cursor_at(Point::new(0.0, 0.0));
        // A `Window(Resized)` event is a real Iced event but
        // doesn't change anything for the canvas; perfect for a
        // no-op input to exercise the redraw-request path.
        let ev = Event::Window(iced::window::Event::Resized(Size::new(420.0, 540.0)));
        let action = p.update(&mut (), &ev, bounds, cursor);
        let (msg, redraw, _status) = action.expect("expected redraw action").into_inner();
        assert!(msg.is_none(), "no message should be published");
        assert!(
            matches!(redraw, iced::window::RedrawRequest::At(_)),
            "expected a redraw-at action, got {redraw:?}"
        );
    }

    #[test]
    fn update_returns_none_when_no_animation() {
        // With no animations registered the canvas should NOT
        // request a redraw — Iced only repaints on state changes
        // or input.
        let mut p = TimelineProgram::new();
        p.snapshot.nodes.push(TimelineNode {
            id: NodeId::new("x"),
            kind: gh_monitor_timeline::NodeKind::Group,
            repo: "x/y".to_string(),
            pairs: vec![],
            time_label: "1 hr ago".to_string(),
            earliest: chrono::Utc::now(),
            latest: chrono::Utc::now(),
            target_url: "https://github.com/x/y".to_string(),
        });
        let bounds = test_bounds();
        let cursor = cursor_at(Point::new(0.0, 0.0));
        let ev = Event::Window(iced::window::Event::Resized(Size::new(420.0, 540.0)));
        let action = p.update(&mut (), &ev, bounds, cursor);
        assert!(
            action.is_none(),
            "expected no redraw action when nothing is animating, got {action:?}"
        );
    }

    #[test]
    fn update_handles_click_first() {
        // When the event is a mouse press on the bell, the click
        // action takes priority over the redraw action — the
        // user must never lose a click to a redraw-while-
        // animating code path.
        let p = make_program_with_anim(NodeAnim::new_insert(Instant::now()));
        let bounds = test_bounds();
        let r = bell_rect(bounds);
        let cursor = cursor_at(Point::new(r.x + r.width / 2.0, r.y + r.height / 2.0));
        let action = p.update(&mut (), &press_event(), bounds, cursor);
        let (msg, _redraw, _status) = action.expect("expected action").into_inner();
        let msg = msg.expect("expected a published message");
        assert!(
            matches!(msg, Message::ToggleNotifications),
            "expected ToggleNotifications, got {msg:?}"
        );
    }

    #[test]
    fn click_outside_bell_falls_through_to_drag() {
        // A click that misses both the bell, the demo button,
        // and any node must publish `Message::DragWindow` (the
        // empty-area path). The animation is active here; the
        // click action still wins.
        let p = make_program_with_anim(NodeAnim::new_insert(Instant::now()));
        let bounds = test_bounds();
        let cursor = cursor_at(Point::new(5.0, 5.0));
        let action = p.update(&mut (), &press_event(), bounds, cursor);
        let (msg, _redraw, _status) = action.expect("expected action").into_inner();
        let msg = msg.expect("expected a published message");
        assert!(
            matches!(msg, Message::DragWindow),
            "expected DragWindow, got {msg:?}"
        );
    }

    #[test]
    fn right_click_opens_context_menu() {
        // A right-click anywhere on the canvas must publish
        // `Message::OpenContextMenu(p)`. The handler is what
        // wires the right-click to the context menu; the canvas
        // does not need to know about `ContextMenu` itself to
        // dispatch this event.
        let p = TimelineProgram::new();
        let bounds = test_bounds();
        let cursor = cursor_at(Point::new(100.0, 200.0));
        let ev = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right));
        let action = p.update(&mut (), &ev, bounds, cursor);
        let (msg, _redraw, _status) = action.expect("expected action").into_inner();
        match msg.expect("expected a published message") {
            Message::OpenContextMenu(p) => {
                assert_eq!(p, Point::new(100.0, 200.0));
            }
            other => panic!("expected OpenContextMenu, got {other:?}"),
        }
    }

    #[test]
    fn demo_button_is_glyph_only_24x24() {
        // The demo button is now a 24x24 glyph-only button (▶) so
        // it doesn't overlap the first node's time label.
        let bounds = test_bounds();
        let r = demo_button_rect(bounds);
        assert_eq!(r.width, 24.0);
        assert_eq!(r.height, 24.0);
        // The button must end within the canvas — it sits flush
        // against the right edge with the configured margin.
        assert!(r.x + r.width <= bounds.width);
        assert!(r.y + r.height <= bounds.height);
    }

    // ---- standalone accent dot tests ----

    #[test]
    fn standalone_dot_color_differs_from_bg() {
        // The standalone bg and the accent dot used to share the
        // same RGB, so the dot was invisible against the bg. The
        // fix is to make the dot white at high alpha. Verify the
        // two are now distinct.
        let opacity = 1.0;
        let bg = node_bg_color(NodeClass::Standalone, opacity);
        let dot = standalone_dot_color(opacity);
        let bg_rgb = (bg.r, bg.g, bg.b);
        let dot_rgb = (dot.r, dot.g, dot.b);
        assert_ne!(
            bg_rgb, dot_rgb,
            "dot rgb {dot_rgb:?} must differ from bg rgb {bg_rgb:?}"
        );
        // The dot is white (1, 1, 1); the bg is gold (1, 0.85, 0.3).
        // They share the red channel but differ in green and blue.
        assert!(
            (dot.r - 1.0).abs() < 0.01,
            "dot must be white, got r={}",
            dot.r
        );
        assert!(
            dot.g > bg.g,
            "dot.g={} must be brighter than bg.g={}",
            dot.g,
            bg.g
        );
        assert!(
            dot.b > bg.b,
            "dot.b={} must be brighter than bg.b={}",
            dot.b,
            bg.b
        );
    }

    #[test]
    fn standalone_dot_alpha_preserves_opacity() {
        // Lower opacity (mid-fade-in) must scale the dot's alpha
        // proportionally so the dot doesn't pop in at full alpha
        // on a half-faded node.
        for opacity in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let dot = standalone_dot_color(opacity);
            assert!(
                (dot.a - opacity * 0.9).abs() < 0.01,
                "alpha {} for opacity {}",
                dot.a,
                opacity
            );
        }
    }

    // ---- time label max-width tests ----

    #[test]
    fn time_label_max_width_is_clamped_to_node() {
        // A narrow node must not allow the time label to bleed
        // left of the node. The clamp is `rect.width - 24.0`
        // (matches the repo label's clamp).
        let wide = NodeRect {
            index: 0,
            x: 12.0,
            y: 12.0,
            width: 396.0,
            height: 56.0,
        };
        let narrow = NodeRect {
            index: 0,
            x: 12.0,
            y: 12.0,
            width: 40.0,
            height: 56.0,
        };
        // Wide node: clamp matches rect.width - 24.
        assert!((time_label_max_width(wide) - 372.0).abs() < 0.01);
        // Narrow node: clamp is still rect.width - 24 (16.0), so
        // the time label can't exceed the node's right edge by
        // more than its right padding.
        assert!((time_label_max_width(narrow) - 16.0).abs() < 0.01);
        // The clamp is never negative.
        let tiny = NodeRect {
            index: 0,
            x: 0.0,
            y: 0.0,
            width: 4.0,
            height: 4.0,
        };
        assert!(time_label_max_width(tiny) >= 0.0);
    }

    // ---- context menu / page tests ----

    fn right_event() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right))
    }

    #[test]
    fn context_menu_rect_is_positioned_at_right_click() {
        // Building a menu from a right-click point and reading its
        // rendered rect must place the menu's top-left at (or
        // near) the click point. This is a no-panic smoke test
        // that exercises the full ContextMenu path the canvas
        // would use in `draw_context_menu`.
        let bounds = test_bounds();
        let menu = ContextMenu::new(Point::new(50.0, 60.0));
        let r = menu.rect(bounds);
        // The menu's top-left should be at (50, 60) — the right
        // click position. The width is the fixed MENU_WIDTH
        // (200.0).
        assert!((r.x - 50.0).abs() < 0.01);
        assert!((r.y - 60.0).abs() < 0.01);
        assert!(r.width > 0.0);
        assert!(r.height > 0.0);
    }

    #[test]
    fn context_menu_layout_supports_walk_for_drawing() {
        // `draw_context_menu` walks the items in order, calling
        // `item_rect(i, bounds)` for each non-separator. This
        // test verifies the walk produces a non-overlapping
        // stack of rects — the precondition for the draw
        // function to render cleanly.
        let bounds = test_bounds();
        let menu = ContextMenu::new(Point::new(0.0, 0.0));
        let menu_rect = menu.rect(bounds);
        // Track the LAST non-separator's rect so we can verify
        // truly-consecutive non-separator rows touch exactly.
        // Reset on every separator — a non-separator after a
        // separator has a gap in between, not a "consecutive"
        // relationship.
        let mut last_non_sep: Option<(f32, f32)> = None;
        // Track the LAST rect (any kind) so we can verify the
        // separator band's gap.
        let mut last_any: Option<(f32, f32)> = None;
        for (i, item) in menu.items.iter().enumerate() {
            if item.is_separator() {
                // Separator band — verify the previous item's
                // bottom and the next item's top leave a gap of
                // exactly SEPARATOR_HEIGHT.
                if let Some((prev_y, prev_h)) = last_any {
                    if let Some(next) = menu.item_rect(i + 1, bounds) {
                        let prev_bottom = prev_y + prev_h;
                        assert!(
                            (next.y - prev_bottom - crate::context_menu::SEPARATOR_HEIGHT).abs()
                                < 0.01,
                            "separator band must be exactly SEPARATOR_HEIGHT wide, \
                             got prev_bottom={prev_bottom} next_top={}",
                            next.y
                        );
                    }
                }
                // Forget the last non-separator so the next
                // non-separator's "consecutive" check starts
                // fresh across the separator.
                last_non_sep = None;
                continue;
            }
            let r = menu
                .item_rect(i, bounds)
                .expect("non-separator must have a rect");
            // Rects must fit inside the menu's outer rect.
            assert!(r.x >= menu_rect.x);
            assert!(r.x + r.width <= menu_rect.x + menu_rect.width + 0.01);
            assert!(r.y >= menu_rect.y);
            assert!(r.y + r.height <= menu_rect.y + menu_rect.height + 0.01);
            if let Some((py, ph)) = last_non_sep {
                // Consecutive non-separator rows must touch
                // exactly (no gap, no overlap).
                assert!(
                    (r.y - (py + ph)).abs() < 0.01,
                    "consecutive non-separator rows must touch, got prev_bottom={} new_top={}",
                    py + ph,
                    r.y
                );
            }
            last_non_sep = Some((r.y, r.height));
            last_any = Some((r.y, r.height));
        }
    }

    #[test]
    fn program_carries_context_menu_field() {
        // The canvas program must hold the new fields so
        // `draw()` can branch on them. This is a smoke test
        // for the wiring.
        let mut p = TimelineProgram::new();
        assert!(p.context_menu.is_none());
        assert!(!p.show_doctor);
        assert!(!p.show_about);
        assert!(p.doctor_results.is_empty());
        assert!(!p.doctor_running);

        p.context_menu = Some(ContextMenu::new(Point::new(0.0, 0.0)));
        p.show_doctor = true;
        p.show_about = true;
        p.doctor_results = vec![CheckResult::ok("config", "ok")];
        p.doctor_running = true;
        assert!(p.context_menu.is_some());
        assert!(p.show_doctor);
        assert!(p.show_about);
        assert_eq!(p.doctor_results.len(), 1);
        assert!(p.doctor_running);
    }

    #[test]
    fn right_click_then_left_click_on_item_publishes_item_message() {
        // A right-click opens the menu; a subsequent left-click
        // on the FIRST item (Settings) must publish
        // `Message::ContextMenuItem(MenuItem::Settings)` so the
        // app opens the settings panel. Without the menu-aware
        // path in `update`, this click would fall through to the
        // empty-area hit-test and publish `Message::DragWindow`.
        let mut p = TimelineProgram::new();
        p.context_menu = Some(ContextMenu::new(Point::new(20.0, 20.0)));
        let bounds = test_bounds();
        let menu = p.context_menu.as_ref().unwrap();
        let item0 = menu
            .item_rect(0, bounds)
            .expect("first item must have a rect");
        let inside_item0 = Point::new(item0.x + item0.width / 2.0, item0.y + item0.height / 2.0);
        let action = p.update(&mut (), &press_event(), bounds, cursor_at(inside_item0));
        let (msg, _redraw, _status) = action.expect("expected action").into_inner();
        let msg = msg.expect("expected a published message");
        assert!(
            matches!(msg, Message::ContextMenuItem(MenuItem::Settings)),
            "expected ContextMenuItem(Settings), got {msg:?}"
        );
    }

    #[test]
    fn left_click_outside_menu_publishes_dismiss_message() {
        // A right-click opens the menu; a subsequent left-click
        // on empty canvas (outside the menu) must publish
        // `Message::DismissContextMenu` so the menu closes. The
        // hit-test must NOT publish `Message::DragWindow` or
        // `Message::OpenUrl` — those are the timeline's job, and
        // the menu is supposed to swallow all clicks while open.
        let mut p = TimelineProgram::new();
        p.context_menu = Some(ContextMenu::new(Point::new(20.0, 20.0)));
        let bounds = test_bounds();
        // Far from the menu's rect — top-left corner of the canvas
        // is well outside the menu.
        let outside = Point::new(2.0, 2.0);
        let action = p.update(&mut (), &press_event(), bounds, cursor_at(outside));
        let (msg, _redraw, _status) = action.expect("expected action").into_inner();
        let msg = msg.expect("expected a published message");
        assert!(
            matches!(msg, Message::DismissContextMenu),
            "expected DismissContextMenu, got {msg:?}"
        );
    }

    #[test]
    fn cursor_moved_over_menu_publishes_hover() {
        // With the menu open, a CursorMoved event over the first
        // item must publish `Message::ContextMenuHover(Some(0))`
        // so the canvas's draw path highlights the item. A
        // CursorMoved outside the menu must publish
        // `Message::ContextMenuHover(None)` so the highlight
        // clears. Without this path the menu is unhoverable.
        let mut p = TimelineProgram::new();
        p.context_menu = Some(ContextMenu::new(Point::new(20.0, 20.0)));
        let bounds = test_bounds();
        let menu = p.context_menu.as_ref().unwrap();
        let item0 = menu
            .item_rect(0, bounds)
            .expect("first item must have a rect");
        let inside = Point::new(item0.x + 4.0, item0.y + 4.0);
        let move_event = Event::Mouse(mouse::Event::CursorMoved { position: inside });
        let action = p.update(&mut (), &move_event, bounds, cursor_at(inside));
        let (msg, _redraw, _status) = action.expect("expected action").into_inner();
        let msg = msg.expect("expected a published message");
        assert!(
            matches!(msg, Message::ContextMenuHover(Some(0))),
            "expected ContextMenuHover(Some(0)), got {msg:?}"
        );

        // Now move outside the menu — the hover must clear.
        let outside = Point::new(2.0, 2.0);
        let action = p.update(&mut (), &move_event, bounds, cursor_at(outside));
        let (msg, _redraw, _status) = action.expect("expected action").into_inner();
        let msg = msg.expect("expected a published message");
        assert!(
            matches!(msg, Message::ContextMenuHover(None)),
            "expected ContextMenuHover(None), got {msg:?}"
        );
    }

    // ---- empty-state / pair-label line-height tests ----

    #[test]
    fn empty_state_line_height_at_least_text_size() {
        // 13pt text needs at least ~16px of line height to
        // breathe; we use 20. The assert is wrapped in a
        // non-constant check so clippy doesn't optimise it away.
        let h: f32 = EMPTY_STATE_LINE_HEIGHT;
        assert!(h >= 20.0, "empty state line height must be at least 20px");
    }

    #[test]
    fn pair_label_line_height_at_least_text_size() {
        // 12pt text needs at least ~14px; we use 16.
        let h: f32 = PAIR_LABEL_LINE_HEIGHT;
        assert!(h >= 16.0, "pair label line height must be at least 16px");
    }
}
