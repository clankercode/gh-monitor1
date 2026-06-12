//! End-to-end integration tests for the timeline pipeline.
//!
//! Takes a list of `RawEvent`s and walks them through `group_by_repo` →
//! `compress` → `TimelineSnapshot::from_compressed` → `diff`. Uses a
//! fixed reference time so snapshots are stable.

use chrono::{Duration, TimeZone, Utc};
use gh_monitor_gh::{EventKind, RawEvent};
use gh_monitor_timeline::snapshot::{diff, TimelineSnapshot};
use gh_monitor_timeline::{compress, group_by_repo, CompressionConfig};

/// Fixed reference "now" used to make the tests deterministic.
fn fixed_now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 13, 12, 0, 0).unwrap()
}

fn ev(repo: &str, kind: EventKind, secs_ago: i64) -> RawEvent {
    RawEvent::for_test(
        repo.to_string(),
        kind,
        fixed_now() - Duration::seconds(secs_ago),
    )
}

fn snapshot_from(events: Vec<RawEvent>) -> TimelineSnapshot {
    let groups = group_by_repo(events);
    let compressed = compress(&groups, &CompressionConfig::default());
    TimelineSnapshot::from_compressed(compressed, fixed_now())
}

#[test]
fn empty_pipeline_yields_empty_snapshot() {
    let s = snapshot_from(vec![]);
    assert!(s.nodes.is_empty());
}

#[test]
fn single_repo_prs_compress() {
    let events = vec![
        ev("a/b", EventKind::PrOpened, 100),
        ev("a/b", EventKind::PrOpened, 50),
    ];
    let s = snapshot_from(events);
    assert_eq!(s.nodes.len(), 1);
    assert_eq!(s.nodes[0].repo, "a/b");
    assert_eq!(s.nodes[0].pairs.len(), 1);
    assert_eq!(s.nodes[0].pairs[0].0.count, 2);
    assert_eq!(s.nodes[0].pairs[0].0.kind, EventKind::PrOpened);
}

#[test]
fn two_repos_stay_separate() {
    let events = vec![
        ev("a/b", EventKind::PrOpened, 100),
        ev("c/d", EventKind::IssueOpened, 50),
    ];
    let s = snapshot_from(events);
    assert_eq!(s.nodes.len(), 2);
    assert_eq!(s.nodes[0].repo, "c/d");
    assert_eq!(s.nodes[1].repo, "a/b");
}

#[test]
fn new_repo_stands_alone() {
    let events = vec![ev("fresh/repo", EventKind::RepoCreated, 30)];
    let s = snapshot_from(events);
    assert_eq!(s.nodes.len(), 1);
    assert!(s.nodes[0].time_label.contains("sec") || s.nodes[0].time_label.contains("min"));
    assert_eq!(s.nodes[0].pairs[0].0.kind, EventKind::RepoCreated);
}

#[test]
fn mixed_kinds_grouped_in_one_node() {
    let events = vec![
        ev("a/b", EventKind::PrOpened, 200),
        ev("a/b", EventKind::PrMerged, 150),
        ev("a/b", EventKind::IssueOpened, 100),
    ];
    let s = snapshot_from(events);
    assert_eq!(
        s.nodes.len(),
        1,
        "all kinds in same repo collapse into one node"
    );
    assert_eq!(s.nodes[0].pairs.len(), 3);
}

#[test]
fn diff_detects_evolution() {
    // Same earliest time → same node id → just an update (count grew).
    let first = snapshot_from(vec![
        ev("a/b", EventKind::PrOpened, 100),
        ev("a/b", EventKind::PrOpened, 60),
    ]);
    let second = snapshot_from(vec![
        ev("a/b", EventKind::PrOpened, 100),
        ev("a/b", EventKind::PrOpened, 60),
        ev("a/b", EventKind::PrOpened, 30),
    ]);
    let d = diff(&first, &second);
    assert!(d.added.is_empty(), "no new nodes, just updated");
    assert_eq!(d.updated.len(), 1, "the existing node's count changed");
    assert!(d.removed.is_empty());
}

#[test]
fn time_label_humanized() {
    let events = vec![
        ev("a/b", EventKind::PrOpened, 60 * 60),      // 1 hr ago
        ev("a/b", EventKind::PrOpened, 60 * 60 + 30), // 1 hr, 30s ago
    ];
    let s = snapshot_from(events);
    assert_eq!(s.nodes.len(), 1);
    let label = &s.nodes[0].time_label;
    assert!(label.contains("hr"), "expected 'hr' in label, got: {label}");
    assert!(
        label.contains("ago"),
        "expected 'ago' in label, got: {label}"
    );
}
