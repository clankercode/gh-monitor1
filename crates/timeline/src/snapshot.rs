//! Point-in-time timeline snapshots and diffs used for animations.

use crate::compress::CompressedNode;
use serde::{Deserialize, Serialize};

/// A stable identifier for a timeline node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub String);

impl NodeId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The kind of node (used for visual treatment).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeKind {
    /// Grouped events (PRs, issues, etc.) — the common case.
    Group,
    /// A rare, important event that stands out (e.g. new repo created).
    Standalone,
}

/// A timeline node in the current view, with the data the renderer needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub repo: String,
    /// Compressed pairs from the source node.
    pub pairs: Vec<(crate::compress::KindCount,)>,
    /// Time range as a humanized label ("1-3 hrs ago", "just now", etc).
    pub time_label: String,
    /// Earliest event time.
    pub earliest: chrono::DateTime<chrono::Utc>,
    /// Latest event time.
    pub latest: chrono::DateTime<chrono::Utc>,
    /// Deep-link target. Clicking the node opens this in the default
    /// browser. Falls back to the repo page when no specific event URL
    /// is available.
    pub target_url: String,
}

impl TimelineNode {
    /// Structural equality for animation diffing. The `time_label`
    /// changes as wall-clock time passes (e.g. "59 mins ago" → "1 hr
    /// ago") without the underlying node having changed, so the diff
    /// must ignore it or every poll would falsely mark every node as
    /// updated.
    pub fn structural_eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.kind == other.kind
            && self.repo == other.repo
            && self.pairs == other.pairs
            && self.earliest == other.earliest
            && self.latest == other.latest
            && self.target_url == other.target_url
    }
}

/// A snapshot of the timeline at a single point in time.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSnapshot {
    /// The nodes in the current snapshot, newest first.
    pub nodes: Vec<TimelineNode>,
}

impl TimelineSnapshot {
    /// Build a snapshot from compressed nodes and a "now" used to compute
    /// the humanized time label for each node.
    pub fn from_compressed(
        compressed: Vec<CompressedNode>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        let nodes = compressed
            .into_iter()
            .map(|n| {
                let time_label = crate::humanize::humanize_range(n.earliest, n.latest, now)
                    .as_str()
                    .to_string();
                TimelineNode {
                    id: NodeId::new(n.id),
                    kind: if n.standalone {
                        NodeKind::Standalone
                    } else {
                        NodeKind::Group
                    },
                    repo: n.repo,
                    pairs: n.pairs.into_iter().map(|kc| (kc,)).collect(),
                    time_label,
                    earliest: n.earliest,
                    latest: n.latest,
                    target_url: n.target_url,
                }
            })
            .collect();
        Self { nodes }
    }
}

/// What happened between two snapshots: a list of new node ids and a list
/// of updated node ids. Used to drive animations (fade-in for new,
/// pulse for updated).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotDiff {
    /// Nodes that exist in `next` but not in `prev` (by id).
    pub added: Vec<NodeId>,
    /// Nodes that exist in both but whose content changed.
    pub updated: Vec<NodeId>,
    /// Nodes that exist in `prev` but not in `next`.
    pub removed: Vec<NodeId>,
}

/// Compute the diff from `prev` to `next`. The "updated" set ignores
/// `time_label` so that a node whose humanized label rolls over ("59
/// mins ago" → "1 hr ago") is not falsely marked as updated on every
/// poll.
pub fn diff(prev: &TimelineSnapshot, next: &TimelineSnapshot) -> SnapshotDiff {
    let prev_by_id: std::collections::HashMap<&NodeId, &TimelineNode> =
        prev.nodes.iter().map(|n| (&n.id, n)).collect();
    let next_by_id: std::collections::HashMap<&NodeId, &TimelineNode> =
        next.nodes.iter().map(|n| (&n.id, n)).collect();

    let added: Vec<NodeId> = next_by_id
        .keys()
        .filter(|id| !prev_by_id.contains_key(*id))
        .cloned()
        .cloned()
        .collect();
    let updated: Vec<NodeId> = next_by_id
        .iter()
        .filter(|(id, n)| match prev_by_id.get(*id) {
            Some(p) => !p.structural_eq(n),
            None => false,
        })
        .map(|(id, _)| (*id).clone())
        .collect();
    let removed: Vec<NodeId> = prev_by_id
        .keys()
        .filter(|id| !next_by_id.contains_key(*id))
        .cloned()
        .cloned()
        .collect();

    SnapshotDiff {
        added,
        updated,
        removed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::{compress, CompressionConfig, KindCount};
    use crate::group::group_by_repo;
    use chrono::Utc;
    use gh_monitor_gh::{EventKind, RawEvent};

    fn ev(repo: &str, kind: EventKind, secs_ago: i64) -> RawEvent {
        let now = Utc::now();
        RawEvent::for_test(
            repo.to_string(),
            kind,
            now - chrono::Duration::seconds(secs_ago),
        )
    }

    #[test]
    fn snapshot_from_compressed() {
        let events = vec![
            ev("a/b", EventKind::PrOpened, 100),
            ev("a/b", EventKind::PrOpened, 50),
        ];
        let groups = group_by_repo(events);
        let compressed = compress(&groups, &CompressionConfig::default());
        let snap = TimelineSnapshot::from_compressed(compressed, Utc::now());
        assert_eq!(snap.nodes.len(), 1);
        assert_eq!(snap.nodes[0].repo, "a/b");
        assert_eq!(snap.nodes[0].pairs[0].0.count, 2);
    }

    #[test]
    fn diff_detects_adds() {
        let n1 = TimelineNode {
            id: NodeId::new("a"),
            kind: NodeKind::Group,
            repo: "x/y".to_string(),
            pairs: vec![(KindCount {
                kind: EventKind::PrOpened,
                count: 1,
            },)],
            time_label: "1 hr ago".to_string(),
            earliest: Utc::now(),
            latest: Utc::now(),
            target_url: "https://github.com/x/y".to_string(),
        };
        let n2 = TimelineNode {
            id: NodeId::new("b"),
            kind: NodeKind::Group,
            repo: "x/y".to_string(),
            pairs: vec![(KindCount {
                kind: EventKind::PrOpened,
                count: 1,
            },)],
            time_label: "1 hr ago".to_string(),
            earliest: Utc::now(),
            latest: Utc::now(),
            target_url: "https://github.com/x/y".to_string(),
        };
        let prev = TimelineSnapshot {
            nodes: vec![n1.clone()],
        };
        let next = TimelineSnapshot {
            nodes: vec![n1, n2],
        };
        let d = diff(&prev, &next);
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.added[0].as_str(), "b");
        assert!(d.updated.is_empty());
        assert!(d.removed.is_empty());
    }

    #[test]
    fn diff_detects_updates() {
        let n1 = TimelineNode {
            id: NodeId::new("a"),
            kind: NodeKind::Group,
            repo: "x/y".to_string(),
            pairs: vec![(KindCount {
                kind: EventKind::PrOpened,
                count: 1,
            },)],
            time_label: "1 hr ago".to_string(),
            earliest: Utc::now(),
            latest: Utc::now(),
            target_url: "https://github.com/x/y".to_string(),
        };
        let mut n2 = n1.clone();
        n2.pairs[0].0.count = 2;
        let prev = TimelineSnapshot { nodes: vec![n1] };
        let next = TimelineSnapshot { nodes: vec![n2] };
        let d = diff(&prev, &next);
        assert!(d.added.is_empty());
        assert_eq!(d.updated.len(), 1);
    }

    #[test]
    fn diff_ignores_time_label_changes() {
        // Two snapshots with the same underlying node but a different
        // humanized `time_label` (e.g. "59 mins ago" → "1 hr ago") must
        // NOT register as an update.
        let n1 = TimelineNode {
            id: NodeId::new("a"),
            kind: NodeKind::Group,
            repo: "x/y".to_string(),
            pairs: vec![(KindCount {
                kind: EventKind::PrOpened,
                count: 1,
            },)],
            time_label: "59 mins ago".to_string(),
            earliest: Utc::now(),
            latest: Utc::now(),
            target_url: "https://github.com/x/y".to_string(),
        };
        let mut n2 = n1.clone();
        n2.time_label = "1 hr ago".to_string();
        let prev = TimelineSnapshot { nodes: vec![n1] };
        let next = TimelineSnapshot { nodes: vec![n2] };
        let d = diff(&prev, &next);
        assert!(d.added.is_empty());
        assert!(
            d.updated.is_empty(),
            "time_label alone must not trigger an update"
        );
        assert!(d.removed.is_empty());
    }

    #[test]
    fn diff_detects_url_change_as_update() {
        // A node whose `target_url` changes (e.g. a new PR replaces an
        // old one in the same group) IS a meaningful update.
        let n1 = TimelineNode {
            id: NodeId::new("a"),
            kind: NodeKind::Group,
            repo: "x/y".to_string(),
            pairs: vec![(KindCount {
                kind: EventKind::PrOpened,
                count: 1,
            },)],
            time_label: "1 hr ago".to_string(),
            earliest: Utc::now(),
            latest: Utc::now(),
            target_url: "https://github.com/x/y/pull/1".to_string(),
        };
        let mut n2 = n1.clone();
        n2.target_url = "https://github.com/x/y/pull/2".to_string();
        let prev = TimelineSnapshot { nodes: vec![n1] };
        let next = TimelineSnapshot { nodes: vec![n2] };
        let d = diff(&prev, &next);
        assert_eq!(d.updated.len(), 1, "url change must trigger an update");
    }

    #[test]
    fn diff_detects_removals() {
        let n1 = TimelineNode {
            id: NodeId::new("a"),
            kind: NodeKind::Group,
            repo: "x/y".to_string(),
            pairs: vec![],
            time_label: "1 hr ago".to_string(),
            earliest: Utc::now(),
            latest: Utc::now(),
            target_url: "https://github.com/x/y".to_string(),
        };
        let prev = TimelineSnapshot { nodes: vec![n1] };
        let next = TimelineSnapshot { nodes: vec![] };
        let d = diff(&prev, &next);
        assert_eq!(d.removed.len(), 1);
    }
}
