//! Right-click context menu: data model and pure layout/hit-test helpers.
//!
//! The menu is drawn by the Iced canvas (see `canvas.rs`) — there is no
//! GTK/muda menu. This module only knows about the items, their
//! geometry, and which one is under a given cursor position; it does
//! not touch Iced's renderer or message types.

use iced::{Point, Rectangle, Size};

/// Row height of a clickable menu item.
pub(crate) const ITEM_HEIGHT: f32 = 28.0;
/// Row height of a non-clickable separator (a thin horizontal line
/// with a few pixels of breathing room above and below).
pub(crate) const SEPARATOR_HEIGHT: f32 = 9.0;
/// Fixed menu width.
pub(crate) const MENU_WIDTH: f32 = 200.0;
/// Inner padding around the row stack.
pub(crate) const MENU_PADDING: f32 = 6.0;

/// The user-facing items in the context menu, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    /// Open the in-pane settings panel.
    Settings,
    /// Toggle the overlay window's `Mode::Hidden` / `Mode::Windowed`
    /// state, same as the tray menu's "Show / Hide" entry.
    ShowHide,
    /// Fire an immediate force-poll, ignoring the regular interval.
    RefreshNow,
    /// Visual divider row. No click target, no hover.
    Separator,
    /// Toggle the in-app Doctor diagnostics page.
    Doctor,
    /// Toggle the About page.
    About,
    /// Quit the app.
    Quit,
}

impl MenuItem {
    /// Human-readable label for the menu row. Empty for separators.
    pub fn label(self) -> &'static str {
        match self {
            MenuItem::Settings => "Settings…",
            MenuItem::ShowHide => "Show / Hide",
            MenuItem::RefreshNow => "Refresh now",
            MenuItem::Doctor => "Doctor…",
            MenuItem::About => "About",
            MenuItem::Quit => "Quit",
            MenuItem::Separator => "",
        }
    }

    /// `true` for the visual divider rows. Separators have no click
    /// target and cannot be hovered.
    pub fn is_separator(self) -> bool {
        matches!(self, MenuItem::Separator)
    }
}

/// The default set of items, in display order. The renderer iterates
/// this list top-to-bottom.
pub const DEFAULT_ITEMS: &[MenuItem] = &[
    MenuItem::Settings,
    MenuItem::ShowHide,
    MenuItem::RefreshNow,
    MenuItem::Separator,
    MenuItem::Doctor,
    MenuItem::About,
    MenuItem::Separator,
    MenuItem::Quit,
];

/// The state of an open context menu. Held by `app::State` and copied
/// into the canvas `Program` on every `sync_program`.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextMenu {
    /// Canvas-local coordinates where the user right-clicked. The
    /// rendered menu may shift to the left or up if the right-click
    /// position would push the menu off the canvas.
    pub position: Point,
    /// Items in display order.
    pub items: Vec<MenuItem>,
    /// Index of the currently hovered item, or `None`. Only set for
    /// non-separator items.
    pub selected: Option<usize>,
}

impl ContextMenu {
    /// Build a menu anchored at `position` with the default item set.
    pub fn new(position: Point) -> Self {
        Self {
            position,
            items: DEFAULT_ITEMS.to_vec(),
            selected: None,
        }
    }

    /// Build a menu anchored at `position` with a custom item list.
    /// Tests use this to drive tiny menus.
    pub fn with_items(position: Point, items: Vec<MenuItem>) -> Self {
        Self {
            position,
            items,
            selected: None,
        }
    }

    /// The total rendered height, including top and bottom padding.
    pub fn total_height(&self) -> f32 {
        let rows: f32 = self
            .items
            .iter()
            .map(|i| {
                if i.is_separator() {
                    SEPARATOR_HEIGHT
                } else {
                    ITEM_HEIGHT
                }
            })
            .sum();
        rows + 2.0 * MENU_PADDING
    }

    /// Compute the menu's bounding rect, clamping to `bounds` so the
    /// menu never overflows the canvas.
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

    /// The rect of a single item row, or `None` for a separator.
    pub fn item_rect(&self, idx: usize, bounds: Rectangle) -> Option<Rectangle> {
        let item = *self.items.get(idx)?;
        if item.is_separator() {
            return None;
        }
        let menu = self.rect(bounds);
        let mut y = menu.y + MENU_PADDING;
        for (i, it) in self.items.iter().enumerate() {
            if i == idx {
                return Some(Rectangle::new(
                    Point::new(menu.x + MENU_PADDING, y),
                    Size::new(menu.width - 2.0 * MENU_PADDING, ITEM_HEIGHT),
                ));
            }
            y += if it.is_separator() {
                SEPARATOR_HEIGHT
            } else {
                ITEM_HEIGHT
            };
        }
        None
    }

    /// Find the index of the item at `cursor_pos`, or `None`. Returns
    /// `None` for separator rows and for clicks outside the menu.
    pub fn item_at(&self, cursor_pos: Point, bounds: Rectangle) -> Option<usize> {
        for (i, _) in self.items.iter().enumerate() {
            if let Some(r) = self.item_rect(i, bounds) {
                if r.contains(cursor_pos) {
                    return Some(i);
                }
            }
        }
        None
    }

    /// `true` if `cursor_pos` is anywhere inside the menu's bounds.
    pub fn contains(&self, cursor_pos: Point, bounds: Rectangle) -> bool {
        self.rect(bounds).contains(cursor_pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> Rectangle {
        Rectangle::new(Point::new(0.0, 0.0), Size::new(400.0, 540.0))
    }

    #[test]
    fn default_menu_has_separators_and_eight_rows() {
        let menu = ContextMenu::new(Point::new(10.0, 20.0));
        assert_eq!(menu.items.len(), 8);
        assert!(menu.items[3].is_separator());
        assert!(menu.items[6].is_separator());
        assert_eq!(menu.items[0], MenuItem::Settings);
        assert_eq!(menu.items[1], MenuItem::ShowHide);
        assert_eq!(menu.items[2], MenuItem::RefreshNow);
        assert_eq!(menu.items[4], MenuItem::Doctor);
        assert_eq!(menu.items[5], MenuItem::About);
        assert_eq!(menu.items[7], MenuItem::Quit);
    }

    #[test]
    fn labels_match_documented_strings() {
        assert_eq!(MenuItem::Settings.label(), "Settings…");
        assert_eq!(MenuItem::ShowHide.label(), "Show / Hide");
        assert_eq!(MenuItem::RefreshNow.label(), "Refresh now");
        assert_eq!(MenuItem::Doctor.label(), "Doctor…");
        assert_eq!(MenuItem::About.label(), "About");
        assert_eq!(MenuItem::Quit.label(), "Quit");
        assert_eq!(MenuItem::Separator.label(), "");
    }

    #[test]
    fn rect_clamps_right_overflow() {
        let menu = ContextMenu::new(Point::new(300.0, 50.0));
        let r = menu.rect(bounds());
        assert_eq!(r.x, 200.0);
        assert_eq!(r.width, MENU_WIDTH);
    }

    #[test]
    fn rect_clamps_bottom_overflow() {
        let menu = ContextMenu::new(Point::new(50.0, 500.0));
        let r = menu.rect(bounds());
        assert!(r.y + r.height <= 540.0 + 0.01);
        assert!(r.y >= 0.0);
    }

    #[test]
    fn item_rect_returns_none_for_separator() {
        let menu = ContextMenu::new(Point::new(10.0, 20.0));
        let b = bounds();
        assert!(menu.item_rect(3, b).is_none());
        assert!(menu.item_rect(6, b).is_none());
    }

    #[test]
    fn item_at_hits_an_item_row() {
        let menu = ContextMenu::new(Point::new(10.0, 20.0));
        let b = bounds();
        let r = menu.item_rect(0, b).expect("first item rect");
        let inside = Point::new(r.x + 4.0, r.y + 4.0);
        assert_eq!(menu.item_at(inside, b), Some(0));
    }

    #[test]
    fn item_at_returns_none_on_separator_band() {
        // Locate the first separator by walking the items, then click
        // inside its band and confirm `item_at` reports `None`. This
        // is robust to the menu's item count changing.
        let menu = ContextMenu::new(Point::new(0.0, 0.0));
        let b = bounds();
        let menu_rect = menu.rect(bounds());
        let mut y = menu_rect.y + MENU_PADDING;
        let mut sep_y: Option<f32> = None;
        for it in &menu.items {
            if it.is_separator() {
                sep_y = Some(y);
                break;
            }
            y += ITEM_HEIGHT;
        }
        let sep_y = sep_y.expect("default menu must contain a separator");
        let inside_sep = Point::new(menu_rect.x + menu_rect.width / 2.0, sep_y + 1.0);
        assert_eq!(menu.item_at(inside_sep, b), None);
    }

    #[test]
    fn item_at_returns_none_for_out_of_bounds_idx() {
        let menu = ContextMenu::new(Point::new(10.0, 20.0));
        let b = bounds();
        assert!(menu.item_at(Point::new(5.0, 5.0), b).is_none());
        assert!(menu.item_rect(99, b).is_none());
    }

    #[test]
    fn contains_inside_and_outside() {
        let menu = ContextMenu::new(Point::new(10.0, 20.0));
        let b = bounds();
        let r = menu.rect(b);
        let inside = Point::new(r.x + 5.0, r.y + 5.0);
        let outside = Point::new(r.x - 1.0, r.y + 5.0);
        assert!(menu.contains(inside, b));
        assert!(!menu.contains(outside, b));
    }

    #[test]
    fn total_height_includes_padding_and_separators() {
        // 6 items * 28 + 2 separators * 9 + 2 * 6 padding
        // = 168 + 18 + 12 = 198.
        let menu = ContextMenu::new(Point::new(0.0, 0.0));
        assert!((menu.total_height() - 198.0).abs() < 0.01);
    }
}
