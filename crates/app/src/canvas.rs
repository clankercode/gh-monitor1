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
        if self.snapshot.nodes.is_empty() {
            draw_empty_state(&mut frame, bounds, self.status.as_deref(), self.needs_setup);
        } else if let Some(s) = &self.status {
            draw_status_banner(&mut frame, bounds, s);
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
                for rect in &rects {
                    if rect.contains(p) {
                        let node = &self.snapshot.nodes[rect.index];
                        return Some(
                            Action::publish(Message::OpenUrl(node.target_url.clone()))
                                .and_capture(),
                        );
                    }
                }
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
}
