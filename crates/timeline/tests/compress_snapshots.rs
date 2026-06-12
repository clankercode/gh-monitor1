//! Snapshot tests for the `compress` output. These pin the human-readable
//! shape of a compressed node so future refactors of the compression
//! algorithm have to either preserve the format or update the snapshot
//! deliberately.

use chrono::{Duration, TimeZone, Utc};
use gh_monitor_gh::{EventKind, RawEvent};
use gh_monitor_timeline::{compress, group_by_repo, CompressionConfig};

/// Fixed reference "now" used to make the snapshots deterministic.
fn fixed_now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 13, 12, 0, 0).unwrap()
}

fn ev(id: &str, repo: &str, kind: EventKind, secs_ago: i64) -> RawEvent {
    RawEvent {
        id: id.to_string(),
        kind,
        repo_full_name: repo.to_string(),
        created_at: fixed_now() - Duration::seconds(secs_ago),
        title: None,
        url: None,
    }
}

/// Pretty-print a list of compressed nodes for the snapshot. The default
/// Debug output is too noisy and unstable to be a useful snapshot.
fn pretty(nodes: &[gh_monitor_timeline::CompressedNode]) -> String {
    let mut s = String::new();
    for n in nodes {
        s.push_str(&format!("- id: {}\n", n.id));
        s.push_str(&format!("  repo: {}\n", n.repo));
        s.push_str(&format!(
            "  span: {} .. {} ({})\n",
            n.earliest.to_rfc3339(),
            n.latest.to_rfc3339(),
            (n.latest - n.earliest).num_seconds()
        ));
        s.push_str(&format!("  standalone: {}\n", n.standalone));
        s.push_str("  pairs:\n");
        for p in &n.pairs {
            s.push_str(&format!("    - kind: {:?}, count: {}\n", p.kind, p.count));
        }
    }
    s
}

#[test]
fn snapshot_single_repo_same_kind() {
    let events = vec![
        ev("e1", "octocat/Hello-World", EventKind::PrOpened, 100),
        ev("e2", "octocat/Hello-World", EventKind::PrOpened, 50),
    ];
    let groups = group_by_repo(events);
    let nodes = compress(&groups, &CompressionConfig::default());
    insta::assert_snapshot!(pretty(&nodes));
}

#[test]
fn snapshot_multi_repo_mixed_kinds() {
    let events = vec![
        ev("e1", "a/b", EventKind::PrOpened, 200),
        ev("e2", "a/b", EventKind::PrMerged, 150),
        ev("e3", "a/b", EventKind::IssueOpened, 100),
        ev("e4", "c/d", EventKind::ReleasePublished, 60),
    ];
    let groups = group_by_repo(events);
    let nodes = compress(&groups, &CompressionConfig::default());
    insta::assert_snapshot!(pretty(&nodes));
}

#[test]
fn snapshot_standalone_repo_created() {
    let events = vec![ev("e1", "fresh/repo", EventKind::RepoCreated, 30)];
    let groups = group_by_repo(events);
    let nodes = compress(&groups, &CompressionConfig::default());
    insta::assert_snapshot!(pretty(&nodes));
}
