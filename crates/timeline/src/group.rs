//! Group raw GitHub events by repo for timeline rendering.

use gh_monitor_gh::RawEvent;
use serde::{Deserialize, Serialize};

/// A group of events from a single repo, in chronological order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoGroup {
    /// "owner/name"
    pub repo: String,
    /// Events in this group, oldest first.
    pub events: Vec<RawEvent>,
}

/// Group `events` by `repo.full_name`. The resulting groups are ordered by
/// their most recent event (newest group first).
pub fn group_by_repo(mut events: Vec<RawEvent>) -> Vec<RepoGroup> {
    events.sort_by_key(|e| std::cmp::Reverse(e.repo_full_name()));

    let mut groups: Vec<RepoGroup> = Vec::new();
    for ev in events {
        if let Some(last) = groups.last_mut() {
            if last.repo == ev.repo_full_name() {
                last.events.push(ev);
                continue;
            }
        }
        groups.push(RepoGroup {
            repo: ev.repo_full_name(),
            events: vec![ev],
        });
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use gh_monitor_gh::{EventKind, RawEvent};

    fn ev(repo: &str, kind: EventKind, secs_ago: i64) -> RawEvent {
        let now = Utc::now();
        RawEvent::for_test(repo.to_string(), kind, now - chrono::Duration::seconds(secs_ago))
    }

    #[test]
    fn groups_same_repo() {
        let events = vec![
            ev("a/b", EventKind::PrOpened, 100),
            ev("a/b", EventKind::PrOpened, 50),
            ev("c/d", EventKind::IssueOpened, 30),
        ];
        let groups = group_by_repo(events);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].repo, "c/d");
        assert_eq!(groups[0].events.len(), 1);
        assert_eq!(groups[1].repo, "a/b");
        assert_eq!(groups[1].events.len(), 2);
    }

    #[test]
    fn empty() {
        let groups = group_by_repo(vec![]);
        assert!(groups.is_empty());
    }

    #[test]
    fn single() {
        let events = vec![ev("a/b", EventKind::PrOpened, 0)];
        let groups = group_by_repo(events);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].repo, "a/b");
    }

    #[test]
    fn preserves_chronological_order_inside_group() {
        let events = vec![
            ev("a/b", EventKind::PrOpened, 200),
            ev("a/b", EventKind::PrOpened, 100),
            ev("a/b", EventKind::PrOpened, 50),
        ];
        let groups = group_by_repo(events);
        assert_eq!(groups[0].events.len(), 3);
        let times: Vec<_> = groups[0].events.iter().map(|e| e.created_at).collect();
        let mut sorted = times.clone();
        sorted.sort();
        assert_eq!(times, sorted, "events should be oldest-first");
    }
}
