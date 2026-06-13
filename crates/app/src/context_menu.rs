//! Right-click context menu: data model and pure layout/hit-test helpers.
//!
//! The menu is drawn by the Iced canvas (see `canvas.rs`) — there is no
//! GTK/muda menu. This module only knows about the items, their
//! geometry, and which one is under a given cursor position; it does
//! not touch Iced's renderer or message types.

use iced::{Point, Rectangle, Size};

/// Row height of a clickable menu item.
pub(crate) const ITEM_HEIGHT: f32 = 28.0;
/// Fixed menu width.
pub(crate) const MENU_WIDTH: f32 = 200.0;
/// Inner padding around the row stack.
pub(crate) const MENU_PADDING: f32 = 6.0;

/// The user-facing items in the context menu, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    /// Open the in-pane settings panel.
    Settings,
    /// Quit the app.
    Quit,
}

impl MenuItem {
    /// Human-readable label for the menu row.
    pub fn label(self) -> &'static str {
        match self {
            MenuItem::Settings => "Settings…",
            MenuItem::Quit => "Quit",
        }
    }
}

/// The default set of items, in display order. The renderer iterates
/// this list top-to-bottom.
pub const DEFAULT_ITEMS: &[MenuItem] = &[MenuItem::Settings, MenuItem::Quit];

/// The state of an open context menu. Held by `app::State` and copied
/// into the canvas `Program` on every `sync_program`.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextMenu {
    pub position: Point,
    pub items: Vec<MenuItem>,
}

impl ContextMenu {
    pub fn new(position: Point) -> Self {
        Self {
            position,
            items: DEFAULT_ITEMS.to_vec(),
        }
    }

    pub fn total_height(&self) -> f32 {
        self.items.len() as f32 * ITEM_HEIGHT + 2.0 * MENU_PADDING
    }

    pub fn rect(&self, bounds: Rectangle) -> Rectangle {
        let width = MENU_WIDTH;
        let height = self.total_height();
        let mut x = self.position.x;
        let mut y = self.position.y;
        if x + width > bounds.width {
            x = (bounds.width - width).max(0.0);
        }
        if y + height > bounds.height {
            y = (bounds.height - height).max(0.0);
        }
        if x < 0.0 {
            x = 0.0;
        }
        if y < 0.0 {
            y = 0.0;
        }
        Rectangle::new(Point::new(x, y), Size::new(width, height))
    }

    pub fn item_rect(&self, idx: usize, bounds: Rectangle) -> Option<Rectangle> {
        let menu = self.rect(bounds);
        let y = menu.y + MENU_PADDING + idx as f32 * ITEM_HEIGHT;
        if idx >= self.items.len() {
            return None;
        }
        Some(Rectangle::new(
            Point::new(menu.x + MENU_PADDING, y),
            Size::new(menu.width - 2.0 * MENU_PADDING, ITEM_HEIGHT),
        ))
    }

    pub fn item_at(&self, cursor_pos: Point, bounds: Rectangle) -> Option<usize> {
        for i in 0..self.items.len() {
            if let Some(r) = self.item_rect(i, bounds) {
                if r.contains(cursor_pos) {
                    return Some(i);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> Rectangle {
        Rectangle::new(Point::new(0.0, 0.0), Size::new(400.0, 540.0))
    }

    #[test]
    fn settings_item_is_in_default_list() {
        let menu = ContextMenu::new(Point::new(10.0, 20.0));
        assert_eq!(menu.items.len(), 2);
        assert_eq!(menu.items[0], MenuItem::Settings);
        assert_eq!(menu.items[1], MenuItem::Quit);
    }

    #[test]
    fn labels_match_documented_strings() {
        assert_eq!(MenuItem::Settings.label(), "Settings…");
        assert_eq!(MenuItem::Quit.label(), "Quit");
    }

    #[test]
    fn item_at_hits_settings_row() {
        let menu = ContextMenu::new(Point::new(10.0, 20.0));
        let b = bounds();
        let r = menu.item_rect(0, b).expect("settings row");
        let inside = Point::new(r.x + 4.0, r.y + 4.0);
        assert_eq!(menu.item_at(inside, b), Some(0));
    }

    #[test]
    fn item_at_hits_quit_row() {
        let menu = ContextMenu::new(Point::new(10.0, 20.0));
        let b = bounds();
        let r = menu.item_rect(1, b).expect("quit row");
        let inside = Point::new(r.x + 4.0, r.y + 4.0);
        assert_eq!(menu.item_at(inside, b), Some(1));
    }
}
