//! The Iced `canvas::Program` that renders the timeline.

use std::collections::HashMap;
use std::time::Instant;

use iced::border;
use iced::mouse::{self, Cursor, Interaction};
use iced::widget::canvas::{self, Action, Frame, Geometry, Path, Program, Stroke};
use iced::{Color, Event, Point, Rectangle, Size, Vector};

use crate::animation::NodeAnim;
use crate::app::Message;
use crate::paint::{layout, NodeClass, NodeRect};
use gh_monitor_timeline::{NodeId, TimelineNode, TimelineSnapshot};

/// Top-right "🎬 Demo" button geometry. The button sits in the
/// chrome strip above the first node and is hit-tested before any
/// node so a click on the button never falls through to a node
/// hit-test. Width/height are in canvas-local units.
const DEMO_BTN_WIDTH: f32 = 76.0;
const DEMO_BTN_HEIGHT: f32 = 24.0;
/// Margin from the canvas edges to the demo button and indicator.
const DEMO_CHROME_MARGIN: f32 = 8.0;
/// Gap between the demo button and the indicator rectangle when
/// both are drawn side by side.
const DEMO_BTN_INDICATOR_GAP: f32 = 8.0;
/// Height of the "Demo running — XXs left" indicator. Same as the
/// button so they share a top row.
const DEMO_INDICATOR_HEIGHT: f32 = 24.0;

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
    /// running. The "🎬 Demo" button is drawn unconditionally; the
    /// indicator is only drawn when this is `Some`. Set from the
    /// app's `sync_program` after every state change.
    pub demo_remaining_secs: Option<u64>,
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

        let (rects, _size) = layout(&self.snapshot, bounds.width);
        let cursor_pos = cursor.position_in(bounds);

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
        let demo_active = self.demo_remaining_secs.is_some();
        if self.snapshot.nodes.is_empty() && !demo_active {
            draw_empty_state(&mut frame, bounds, self.status.as_deref(), self.needs_setup);
        } else if let Some(s) = &self.status {
            draw_status_banner(&mut frame, bounds, s);
        }

        // Demo chrome: the "🎬 Demo" button is always drawn in the
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
        let (rects, _) = layout(&self.snapshot, bounds.width);
        let cursor_pos = cursor.position_in(bounds);

        if let Some(p) = cursor_pos {
            if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event {
                // Demo button first — it sits in the chrome strip
                // above the first node and must take priority over
                // any node that might happen to overlap (the status
                // banner overlays the same area).
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
        None
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        let (rects, _) = layout(&self.snapshot, bounds.width);
        if let Some(p) = cursor.position_in(bounds) {
            // Demo button takes priority over nodes — the button
            // lives in the chrome strip that overlaps the first
            // node, so a node hit-test alone would show a pointer
            // cursor even when the cursor is on the button.
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

/// Draw the "🎬 Demo" button in the top-right corner. The button
/// is always visible (even when no demo is running) so the user
/// knows the feature exists. The text is right-aligned within the
/// button with a small inner padding.
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
    frame.fill_text(canvas::Text {
        content: "🎬 Demo".to_string(),
        position: Point::new(rect.x + rect.width - 10.0, rect.y + 5.0),
        max_width: rect.width - 14.0,
        color: Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 0.95,
        },
        size: 12.0.into(),
        align_x: iced::alignment::Horizontal::Right.into(),
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
    let mut y = bounds.height / 2.0 - (lines.len() as f32 * 18.0) / 2.0;
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
        y += 18.0;
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

fn draw_node(
    frame: &mut Frame,
    node: &TimelineNode,
    rect: NodeRect,
    opacity: f32,
    pulse: f32,
    hovering: bool,
    class: NodeClass,
) {
    let bg_color = match class {
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
    };
    let bg = Path::rounded_rectangle(
        Point::new(rect.x, rect.y),
        Size::new(rect.width, rect.height),
        border::Radius::new(8.0),
    );
    frame.fill(&bg, bg_color);

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
        y += 14.0;
    }

    // Time label (top-right).
    frame.fill_text(canvas::Text {
        content: node.time_label.clone(),
        position: Point::new(rect.x + rect.width - 12.0, rect.y + 8.0),
        max_width: 200.0,
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

    // Standalone accent dot.
    if matches!(class, NodeClass::Standalone) {
        let dot = Path::circle(Point::new(rect.x + rect.width - 14.0, rect.y + 26.0), 3.0);
        frame.fill(
            &dot,
            Color {
                r: 1.0,
                g: 0.85,
                b: 0.30,
                a: opacity,
            },
        );
    }

    // Suppress unused warning for Vector.
    let _ = Vector::new(0.0, 0.0);
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(r.x, 420.0 - 8.0 - 76.0);
        assert_eq!(r.y, 8.0);
        assert_eq!(r.width, 76.0);
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
}
