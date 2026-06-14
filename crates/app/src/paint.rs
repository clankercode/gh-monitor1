//! Pure painting/layout helpers for the timeline canvas. Separated from
//! the Iced `Program` impl so we can unit-test the layout and URL logic
//! without a display.

#[allow(unused_imports)]
use gh_monitor_timeline::{NodeKind, TimelineNode, TimelineSnapshot};

/// Per-node rectangle in canvas-local coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeRect {
    /// Index into the snapshot's `nodes` vec.
    pub index: usize,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl NodeRect {
    pub fn contains(&self, p: iced::Point) -> bool {
        p.x >= self.x && p.x <= self.x + self.width && p.y >= self.y && p.y <= self.y + self.height
    }
}

/// Total dimensions of the canvas, after laying out the snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CanvasSize {
    pub width: f32,
    pub height: f32,
}

const NODE_HEIGHT: f32 = 56.0;
const NODE_GAP: f32 = 8.0;
const PADDING: f32 = 12.0;
/// Vertical offset reserved for the status banner at the top of the
/// canvas. When the banner is visible the first node starts at
/// `PADDING + STATUS_BANNER_RESERVED` so the banner (y=4..36) does
/// not overlap the first node.
const STATUS_BANNER_RESERVED: f32 = 32.0;

/// Layout the snapshot's nodes into rectangles. Top-down, fixed
/// height per node, single column. The total height is computed.
/// When `has_status_banner` is `true` the first node starts
/// `STATUS_BANNER_RESERVED` pixels lower so the status banner
/// (rendered separately at y=4..36) does not overlap it.
pub fn layout(
    snapshot: &TimelineSnapshot,
    max_width: f32,
    has_status_banner: bool,
) -> (Vec<NodeRect>, CanvasSize) {
    let mut rects = Vec::with_capacity(snapshot.nodes.len());
    let width = max_width;
    let mut y = PADDING;
    if has_status_banner {
        y += STATUS_BANNER_RESERVED;
    }
    for (i, _node) in snapshot.nodes.iter().enumerate() {
        rects.push(NodeRect {
            index: i,
            x: PADDING,
            y,
            width: width - 2.0 * PADDING,
            height: NODE_HEIGHT,
        });
        y += NODE_HEIGHT + NODE_GAP;
    }
    let total_height = y + PADDING;
    (
        rects,
        CanvasSize {
            width,
            height: total_height,
        },
    )
}

/// Build a fallback deep-link URL for a node, pointing at the repo's
/// activity page. The canvas no longer uses this — each node carries
/// its own `target_url` from the source event — but it's kept here as a
/// helper for tests and other call sites that only have a repo name.
#[allow(dead_code)]
pub fn url_for_repo(repo: &str) -> String {
    format!("https://github.com/{repo}")
}

/// Visual class for a node, used to pick colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeClass {
    /// A grouped (PRs, issues) node — muted.
    Group,
    /// A standalone (new repo) node — accented.
    Standalone,
}

impl NodeClass {
    pub fn from_node_kind(kind: NodeKind) -> Self {
        match kind {
            NodeKind::Group => Self::Group,
            NodeKind::Standalone => Self::Standalone,
        }
    }
}

/// Human-readable label for a (kind, count) pair, e.g. "3 PRs opened".
pub fn pair_label(kind: gh_monitor_gh::EventKind, count: u32) -> String {
    let (singular, plural) = match kind {
        gh_monitor_gh::EventKind::PrOpened => ("PR opened", "PRs opened"),
        gh_monitor_gh::EventKind::PrMerged => ("PR merged", "PRs merged"),
        gh_monitor_gh::EventKind::IssueOpened => ("issue opened", "issues opened"),
        gh_monitor_gh::EventKind::ReleasePublished => ("release", "releases"),
        gh_monitor_gh::EventKind::RepoCreated => ("new repo", "new repos"),
    };
    let word = if count == 1 { singular } else { plural };
    format!("{count} {word}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use gh_monitor_gh::EventKind;

    fn node(repo: &str, kind: NodeKind) -> TimelineNode {
        TimelineNode {
            id: gh_monitor_timeline::NodeId::new(format!("{repo}:{kind:?}")),
            kind,
            repo: repo.to_string(),
            pairs: vec![(gh_monitor_timeline::compress::KindCount {
                kind: EventKind::PrOpened,
                count: 1,
            },)],
            time_label: "1 hr ago".to_string(),
            earliest: Utc::now(),
            latest: Utc::now(),
            target_url: format!("https://github.com/{repo}"),
        }
    }

    #[test]
    fn layout_empty() {
        let snap = TimelineSnapshot::default();
        let (rects, size) = layout(&snap, 400.0, false);
        assert!(rects.is_empty());
        assert!(size.height >= PADDING);
    }

    #[test]
    fn layout_three_nodes() {
        let snap = TimelineSnapshot {
            nodes: vec![
                node("a/b", NodeKind::Group),
                node("c/d", NodeKind::Group),
                node("e/f", NodeKind::Group),
            ],
        };
        let (rects, size) = layout(&snap, 400.0, false);
        assert_eq!(rects.len(), 3);
        assert!(rects[0].y < rects[1].y);
        assert!(rects[1].y < rects[2].y);
        let expected_height = PADDING * 2.0 + 3.0 * NODE_HEIGHT + 3.0 * NODE_GAP;
        assert!((size.height - expected_height).abs() < 0.01);
    }

    #[test]
    fn layout_pushes_first_node_down_when_status_banner_set() {
        // The status banner sits at y=4..36. With it visible the
        // first node must start BELOW the banner (y >= 36) so it
        // doesn't get covered. Without the banner the first node
        // starts at PADDING.
        let snap = TimelineSnapshot {
            nodes: vec![node("a/b", NodeKind::Group)],
        };
        let (with_banner, _) = layout(&snap, 400.0, true);
        let (no_banner, _) = layout(&snap, 400.0, false);
        assert!(
            (with_banner[0].y - no_banner[0].y - STATUS_BANNER_RESERVED).abs() < 0.01,
            "banner offset must equal STATUS_BANNER_RESERVED, got {} vs {}",
            with_banner[0].y,
            no_banner[0].y
        );
        assert!(
            with_banner[0].y >= 36.0,
            "first node must start at or below the banner's bottom edge (36), got {}",
            with_banner[0].y
        );
    }

    #[test]
    fn hit_test_inside() {
        let snap = TimelineSnapshot {
            nodes: vec![node("a/b", NodeKind::Group)],
        };
        let (rects, _) = layout(&snap, 400.0, false);
        let inside = iced::Point::new(rects[0].x + 5.0, rects[0].y + 5.0);
        let outside = iced::Point::new(0.0, 0.0);
        assert!(rects[0].contains(inside));
        assert!(!rects[0].contains(outside));
    }

    #[test]
    fn url_for_repo_uses_repo_name() {
        assert_eq!(
            url_for_repo("octocat/Hello-World"),
            "https://github.com/octocat/Hello-World"
        );
    }

    #[test]
    fn pair_label_singular_and_plural() {
        assert_eq!(pair_label(EventKind::PrOpened, 1), "1 PR opened");
        assert_eq!(pair_label(EventKind::PrOpened, 3), "3 PRs opened");
        assert_eq!(pair_label(EventKind::IssueOpened, 2), "2 issues opened");
        assert_eq!(pair_label(EventKind::ReleasePublished, 1), "1 release");
        assert_eq!(pair_label(EventKind::ReleasePublished, 2), "2 releases");
    }
}
