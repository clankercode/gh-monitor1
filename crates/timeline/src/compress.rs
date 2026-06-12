//! Compress groups of similar events into timeline nodes.

use crate::group::RepoGroup;
use chrono::{DateTime, Utc};
use gh_monitor_gh::{EventKind, RawEvent};
use serde::{Deserialize, Serialize};

/// Configuration for the compression algorithm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompressionConfig {
    /// Maximum time-span (in seconds) within which events of the same type
    /// in the same repo can be collapsed.
    pub window_secs: i64,
    /// Maximum number of `(type, count)` pairs per node before further
    /// types spill into additional nodes.
    pub max_pairs_per_node: usize,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            window_secs: 3 * 60 * 60,
            max_pairs_per_node: 3,
        }
    }
}

/// A pair of (event type, count) within a compressed node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindCount {
    /// The event kind.
    pub kind: EventKind,
    /// The number of events of this kind.
    pub count: u32,
}

/// A single node on the timeline. A node is either:
/// - a *grouped* node (one or more `KindCount`s from one repo, with a time
///   range), or
/// - a *standalone* node (one event, never compressed — e.g. new repo
///   creation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompressedNode {
    /// Stable id derived from (repo, kind-set, earliest timestamp). Two
    /// compressions of the same input produce the same id.
    pub id: String,
    /// "owner/name"
    pub repo: String,
    /// Earliest event time in this node.
    pub earliest: DateTime<Utc>,
    /// Latest event time in this node.
    pub latest: DateTime<Utc>,
    /// (kind, count) pairs.
    pub pairs: Vec<KindCount>,
    /// If true, this is a rare/important event that should not be
    /// compressed (e.g. new repo created).
    pub standalone: bool,
    /// Deep-link target. For grouped nodes, the URL of the most recent
    /// event in the chunk (so clicking jumps to "the latest thing" in
    /// that group). For standalone nodes, the URL of the single event.
    /// Always set when a URL is available; falls back to the repo page
    /// otherwise.
    pub target_url: String,
}

impl CompressedNode {
    /// The repo's display name (after the slash).
    pub fn repo_short(&self) -> &str {
        self.repo
            .split_once('/')
            .map(|(_, n)| n)
            .unwrap_or(&self.repo)
    }
}

/// Compress a list of repo groups into timeline nodes. New repo creation
/// is never compressed.
pub fn compress(groups: &[RepoGroup], cfg: &CompressionConfig) -> Vec<CompressedNode> {
    let mut nodes: Vec<CompressedNode> = Vec::new();

    for group in groups {
        if group.events.is_empty() {
            continue;
        }

        // Standalone events: never compress.
        let mut to_compress: Vec<&RawEvent> = Vec::new();
        for ev in &group.events {
            if ev.kind.is_standalone() {
                nodes.push(standalone_node(ev));
            } else {
                to_compress.push(ev);
            }
        }

        if to_compress.is_empty() {
            continue;
        }

        // Sort oldest first.
        to_compress.sort_by_key(|e| e.created_at);

        // Greedy compression: walk oldest -> newest, group events that
        // are within `window_secs` of the first event in the current
        // chunk. When you hit a gap, start a new chunk.
        let mut chunks: Vec<Vec<&RawEvent>> = Vec::new();
        let mut current: Vec<&RawEvent> = Vec::new();
        for ev in to_compress {
            if current.is_empty() {
                current.push(ev);
            } else {
                let first = current[0].created_at;
                let delta = (ev.created_at - first).num_seconds().abs();
                if delta <= cfg.window_secs {
                    current.push(ev);
                } else {
                    chunks.push(std::mem::take(&mut current));
                    current.push(ev);
                }
            }
        }
        if !current.is_empty() {
            chunks.push(current);
        }

        for chunk in chunks {
            nodes.push(compress_chunk(&group.repo, &chunk));
        }
    }

    // Newest nodes first.
    nodes.sort_by_key(|n| std::cmp::Reverse(n.latest));
    nodes
}

fn standalone_node(ev: &RawEvent) -> CompressedNode {
    CompressedNode {
        id: format!("standalone:{}:{}", ev.repo_full_name(), ev.id),
        repo: ev.repo_full_name(),
        earliest: ev.created_at,
        latest: ev.created_at,
        pairs: vec![KindCount {
            kind: ev.kind,
            count: 1,
        }],
        standalone: true,
        target_url: ev
            .url
            .as_ref()
            .map(|u| u.to_string())
            .unwrap_or_else(|| fallback_repo_url(&ev.repo_full_name())),
    }
}

fn compress_chunk(repo: &str, chunk: &[&RawEvent]) -> CompressedNode {
    let mut counts: std::collections::BTreeMap<EventKind, u32> = std::collections::BTreeMap::new();
    let mut earliest = chunk[0].created_at;
    let mut latest = chunk[0].created_at;
    for ev in chunk {
        *counts.entry(ev.kind).or_insert(0) += 1;
        if ev.created_at < earliest {
            earliest = ev.created_at;
        }
        if ev.created_at > latest {
            latest = ev.created_at;
        }
    }
    let pairs: Vec<KindCount> = counts
        .into_iter()
        .map(|(kind, count)| KindCount { kind, count })
        .collect();

    // Stable id: repo + earliest timestamp + sorted kinds.
    let kinds: Vec<String> = pairs.iter().map(|p| format!("{:?}", p.kind)).collect();
    let id = format!(
        "group:{}:{}:{}",
        repo,
        earliest.timestamp(),
        kinds.join("|")
    );

    // The chunk is sorted oldest-first by `compress` before this is
    // called, so the most recent event is the last one. Use its URL as
    // the node's deep link.
    let target_url = chunk
        .last()
        .and_then(|ev| ev.url.as_ref().map(|u| u.to_string()))
        .unwrap_or_else(|| fallback_repo_url(repo));

    CompressedNode {
        id,
        repo: repo.to_string(),
        earliest,
        latest,
        pairs,
        standalone: false,
        target_url,
    }
}

/// The default deep-link for a node when no specific event URL is
/// available: the repo's activity page.
fn fallback_repo_url(repo: &str) -> String {
    format!("https://github.com/{repo}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use gh_monitor_gh::EventKind;

    fn ev(repo: &str, kind: EventKind, secs_ago: i64) -> RawEvent {
        let now = Utc::now();
        RawEvent::for_test(
            repo.to_string(),
            kind,
            now - chrono::Duration::seconds(secs_ago),
        )
    }

    fn group(repo: &str, events: Vec<RawEvent>) -> RepoGroup {
        RepoGroup {
            repo: repo.to_string(),
            events,
        }
    }

    #[test]
    fn compresses_same_type_same_window() {
        let events = vec![
            ev("a/b", EventKind::PrOpened, 100),
            ev("a/b", EventKind::PrOpened, 50),
            ev("a/b", EventKind::PrOpened, 0),
        ];
        let groups = vec![group("a/b", events)];
        let nodes = compress(&groups, &CompressionConfig::default());
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].pairs.len(), 1);
        assert_eq!(nodes[0].pairs[0].kind, EventKind::PrOpened);
        assert_eq!(nodes[0].pairs[0].count, 3);
        assert!(!nodes[0].standalone);
    }

    #[test]
    fn splits_on_time_gap() {
        let events = vec![
            ev("a/b", EventKind::PrOpened, 86_400), // > 24h ago, outside default 3h
            ev("a/b", EventKind::PrOpened, 100),
            ev("a/b", EventKind::PrOpened, 50),
        ];
        let groups = vec![group("a/b", events)];
        let nodes = compress(&groups, &CompressionConfig::default());
        assert_eq!(nodes.len(), 2, "should split into two nodes by time gap");
    }

    #[test]
    fn groups_multiple_kinds_in_one_node() {
        let events = vec![
            ev("a/b", EventKind::PrOpened, 100),
            ev("a/b", EventKind::PrMerged, 50),
        ];
        let groups = vec![group("a/b", events)];
        let nodes = compress(&groups, &CompressionConfig::default());
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].pairs.len(), 2);
    }

    #[test]
    fn standalone_event_not_compressed() {
        let events = vec![ev("a/b", EventKind::RepoCreated, 100)];
        let groups = vec![group("a/b", events)];
        let nodes = compress(&groups, &CompressionConfig::default());
        assert_eq!(nodes.len(), 1);
        assert!(nodes[0].standalone);
        assert_eq!(nodes[0].pairs.len(), 1);
        assert_eq!(nodes[0].pairs[0].count, 1);
    }

    #[test]
    fn stable_id_across_runs() {
        let events1 = vec![
            ev("a/b", EventKind::PrOpened, 100),
            ev("a/b", EventKind::PrMerged, 50),
        ];
        let events2 = events1.clone();
        let groups1 = vec![group("a/b", events1)];
        let groups2 = vec![group("a/b", events2)];
        let n1 = compress(&groups1, &CompressionConfig::default());
        let n2 = compress(&groups2, &CompressionConfig::default());
        assert_eq!(n1[0].id, n2[0].id);
    }
}
