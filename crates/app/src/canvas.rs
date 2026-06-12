//! The Iced `canvas::Program` that renders the timeline.

use std::collections::HashMap;
use std::time::Instant;

use iced::border;
use iced::mouse::{self, Cursor, Interaction};
use iced::widget::canvas::{self, Action, Frame, Geometry, Path, Program, Stroke};
use iced::{Color, Event, Point, Rectangle, Size, Vector};

use crate::animation::NodeAnim;
use crate::app::Message;
use crate::paint::{layout, url_for_node, NodeClass, NodeRect};
use gh_monitor_timeline::{NodeId, TimelineNode, TimelineSnapshot};

/// The canvas program. Holds the current snapshot, the per-node animation
/// state, and the current window id.
#[derive(Debug)]
pub struct TimelineProgram {
    pub snapshot: TimelineSnapshot,
    pub anims: HashMap<NodeId, NodeAnim>,
    pub window_id: Option<iced::window::Id>,
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
        }
    }

    pub fn update_snapshot(&mut self, snap: TimelineSnapshot) {
        self.snapshot = snap;
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
        theme: &iced::Theme,
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
            let hovering = cursor_pos.is_some() && rect.contains(cursor_pos.unwrap());
            draw_node(
                &mut frame,
                node,
                rect,
                opacity,
                pulse,
                hovering,
                NodeClass::from_node_kind(node.kind),
                theme,
            );
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
                        let url = url_for_node(node);
                        return Some(Action::publish(Message::OpenUrl(url)).and_capture());
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

fn draw_node(
    frame: &mut Frame,
    node: &TimelineNode,
    rect: NodeRect,
    opacity: f32,
    pulse: f32,
    hovering: bool,
    class: NodeClass,
    _theme: &iced::Theme,
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
