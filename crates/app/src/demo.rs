//! Demo mode: a scripted sequence of fake GitHub events.
//!
//! When the user clicks the "🎬 Demo" button on the canvas, the app
//! enters demo mode: it clears the timeline and replays a
//! [`DEMO_TOTAL_SECS`]-second scripted sequence of fake events. The
//! sequence exercises every animation path:
//!
//! - new-node fade-in (a brand-new repo appears on the timeline),
//! - update pulse (a count goes from "2 PRs opened" to "3 PRs opened"),
//! - compressed grouping (three PRs in the same repo within the time
//!   window share a single `(kind, count)` node),
//! - standalone visual (a `RepoCreated` event gets a distinct accent
//!   and is never compressed into a group).
//!
//! Once the script completes the final state is left visible; the
//! user can click the button again to re-run from a clean slate.

use std::time::{Duration, Instant};

use gh_monitor_gh::{EventKind, RawEvent};

/// Total demo window, in seconds. After the last scripted event
/// fires (around `t=10s` with the default script), the demo state
/// stays active for the remainder so the "Demo running — XXs left"
/// indicator is useful and the user has time to look at the final
/// state. Once the window elapses the demo state is cleared.
pub const DEMO_TOTAL_SECS: u64 = 120;

/// State for a running demo. `None` on the app's `State` when no
/// demo is active. Holds the schedule and a cursor for the next
/// event to fire.
#[derive(Debug)]
pub(crate) struct DemoState {
    /// When the demo started.
    pub(crate) started_at: Instant,
    /// Scheduled events, in firing order. Each entry fires at the
    /// absolute `Instant` recorded here.
    pub(crate) events: Vec<(Instant, Vec<RawEvent>)>,
    /// Index of the next event to fire. `events.len()` once all
    /// events have fired.
    pub(crate) next_idx: usize,
}

impl DemoState {
    /// Build a demo state starting at `Instant::now()`.
    pub(crate) fn new() -> Self {
        Self::new_at(Instant::now())
    }

    /// Build a demo state with a known start time. Used in tests
    /// where the `Instant` is captured for deterministic assertions
    /// against `drain_due`.
    pub(crate) fn new_at(started_at: Instant) -> Self {
        // Use a single "now" for the events' `created_at` so the
        // humanized time labels look consistent within one demo
        // run.
        let chrono_now = chrono::Utc::now();
        let mk = |counter: u32,
                  repo: &'static str,
                  kind: EventKind,
                  secs_ago: i64,
                  title: Option<&'static str>,
                  url: Option<&'static str>|
         -> RawEvent {
            let mut ev = RawEvent::for_test(
                repo.to_string(),
                kind,
                chrono_now - chrono::Duration::seconds(secs_ago),
            );
            ev.id = format!("demo-{counter}");
            ev.title = title.map(String::from);
            if let Some(s) = url {
                ev.url = Some(url::Url::parse(s).expect("hard-coded demo URL must be valid"));
            }
            ev
        };

        // The script: 10 events across 4 repos. Each entry fires
        // 1.0s after the previous, starting at t=1.0s. With 10
        // events the last fires at t=10.0s; the demo window stays
        // active for the remaining `DEMO_TOTAL_SECS - 10` seconds
        // so the "Demo running — XXs left" indicator is useful.
        //
        // Animation paths exercised:
        //   t=1: rust-lang/rust: PrOpened              new (fade in)
        //   t=2: tokio-rs/tokio: PrOpened              new (fade in)
        //   t=3: rust-lang/rust: PrOpened              update pulse (1 -> 2)
        //   t=4: rust-lang/rust: PrOpened              update pulse (2 -> 3)
        //   t=5: rust-lang/rust: PrMerged              new kinds, new node
        //   t=6: acme-corp/secret-project: RepoCreated standalone
        //   t=7: tokio-rs/tokio: IssueOpened           new kinds, new node
        //   t=8: serde-rs/serde: ReleasePublished       new (fade in)
        //   t=9: rust-lang/rust: IssueOpened           new kinds, new node
        //   t=10: tokio-rs/tokio: PrOpened             update pulse (1 -> 2)
        let script: Vec<(f64, RawEvent)> = vec![
            (
                1.0,
                mk(
                    1,
                    "rust-lang/rust",
                    EventKind::PrOpened,
                    60,
                    Some("Stabilize u64::div_ceil"),
                    Some("https://github.com/rust-lang/rust/pull/1"),
                ),
            ),
            (
                2.0,
                mk(
                    2,
                    "tokio-rs/tokio",
                    EventKind::PrOpened,
                    50,
                    Some("Improve scheduler fairness"),
                    Some("https://github.com/tokio-rs/tokio/pull/100"),
                ),
            ),
            (
                3.0,
                mk(
                    3,
                    "rust-lang/rust",
                    EventKind::PrOpened,
                    40,
                    Some("Add Iterator::map_while"),
                    Some("https://github.com/rust-lang/rust/pull/2"),
                ),
            ),
            (
                4.0,
                mk(
                    4,
                    "rust-lang/rust",
                    EventKind::PrOpened,
                    30,
                    Some("Remove deprecated feature"),
                    Some("https://github.com/rust-lang/rust/pull/3"),
                ),
            ),
            (
                5.0,
                mk(
                    5,
                    "rust-lang/rust",
                    EventKind::PrMerged,
                    20,
                    Some("Fix borrowck NLL bug"),
                    Some("https://github.com/rust-lang/rust/pull/4"),
                ),
            ),
            (
                6.0,
                mk(
                    6,
                    "acme-corp/secret-project",
                    EventKind::RepoCreated,
                    10,
                    None,
                    None,
                ),
            ),
            (
                7.0,
                mk(
                    7,
                    "tokio-rs/tokio",
                    EventKind::IssueOpened,
                    8,
                    Some("Scheduler panic on overflow"),
                    Some("https://github.com/tokio-rs/tokio/issues/50"),
                ),
            ),
            (
                8.0,
                mk(
                    8,
                    "serde-rs/serde",
                    EventKind::ReleasePublished,
                    5,
                    Some("serde-1.0.215"),
                    Some("https://github.com/serde-rs/serde/releases/tag/v1.0.215"),
                ),
            ),
            (
                9.0,
                mk(
                    9,
                    "rust-lang/rust",
                    EventKind::IssueOpened,
                    3,
                    Some("Compiler panic on infinite loop"),
                    Some("https://github.com/rust-lang/rust/issues/5"),
                ),
            ),
            (
                10.0,
                mk(
                    10,
                    "tokio-rs/tokio",
                    EventKind::PrOpened,
                    1,
                    Some("Reduce allocation in mpsc::Sender"),
                    Some("https://github.com/tokio-rs/tokio/pull/101"),
                ),
            ),
        ];

        let events: Vec<(Instant, Vec<RawEvent>)> = script
            .into_iter()
            .map(|(offset_secs, ev)| (started_at + Duration::from_secs_f64(offset_secs), vec![ev]))
            .collect();

        Self {
            started_at,
            events,
            next_idx: 0,
        }
    }

    /// Drain all events whose scheduled time is `<= now`. Advances
    /// `next_idx` past the drained events so a subsequent call
    /// doesn't re-emit them. Returns an empty vec when nothing is
    /// due or when the schedule has been fully drained.
    pub(crate) fn drain_due(&mut self, now: Instant) -> Vec<RawEvent> {
        let mut out = Vec::new();
        while let Some((at, evs)) = self.events.get(self.next_idx) {
            if *at <= now {
                out.extend(evs.iter().cloned());
                self.next_idx += 1;
            } else {
                break;
            }
        }
        out
    }

    /// Seconds remaining in the demo (clamped to 0). Used to render
    /// the "Demo running — XXs left" indicator. Monotonic: a value
    /// passed in before `started_at` (e.g. the demo script was
    /// constructed with a future `Instant` in a test) saturates to
    /// `DEMO_TOTAL_SECS`.
    pub(crate) fn remaining_secs(&self, now: Instant) -> u64 {
        let elapsed = now.saturating_duration_since(self.started_at).as_secs();
        DEMO_TOTAL_SECS.saturating_sub(elapsed)
    }

    /// `true` when the demo window has elapsed and the state should
    /// be cleared. After this returns `true` the only correct
    /// action is to drop the `DemoState`.
    pub(crate) fn is_complete(&self, now: Instant) -> bool {
        self.remaining_secs(now) == 0
    }

    /// Total number of scheduled events. Used by tests to assert
    /// the script's size.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.events.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gh_monitor_gh::EventKind;

    #[test]
    fn script_has_ten_events_across_four_repos() {
        // The demo should cover multiple repos and exercise all
        // event kinds (5 kinds × at least one occurrence).
        let started = Instant::now();
        let mut demo = DemoState::new_at(started);
        assert_eq!(demo.len(), 10, "script should have 10 events");

        // Drain everything by passing a time well past the last
        // event (one second past t=10s).
        let all = demo.drain_due(started + Duration::from_secs(11));
        assert_eq!(all.len(), 10);

        let mut repos: Vec<&str> = all.iter().map(|e| e.repo_full_name.as_str()).collect();
        repos.sort();
        repos.dedup();
        assert_eq!(repos.len(), 4, "expected 4 distinct repos, got {repos:?}");

        let mut kinds: Vec<EventKind> = all.iter().map(|e| e.kind).collect();
        kinds.sort();
        kinds.dedup();
        assert_eq!(
            kinds.len(),
            5,
            "expected all 5 event kinds to appear, got {kinds:?}"
        );
        assert!(kinds.contains(&EventKind::RepoCreated));
    }

    #[test]
    fn events_fire_at_one_second_intervals() {
        // Each event should be scheduled exactly 1.0s after the
        // previous, starting at t=1.0s.
        let started = Instant::now();
        let demo = DemoState::new_at(started);
        let offsets: Vec<Duration> = demo
            .events
            .iter()
            .map(|(at, _)| at.saturating_duration_since(started))
            .collect();
        for (i, off) in offsets.iter().enumerate() {
            let expected = Duration::from_secs((i as u64) + 1);
            assert_eq!(
                *off, expected,
                "event {i} should fire at {expected:?}, got {off:?}"
            );
        }
    }

    #[test]
    fn drain_due_returns_nothing_before_first_event() {
        let started = Instant::now();
        let mut demo = DemoState::new_at(started);
        // At t=0 (the started_at itself) and t=0.5s, nothing has
        // fired yet.
        assert!(demo.drain_due(started).is_empty());
        assert!(demo
            .drain_due(started + Duration::from_millis(500))
            .is_empty());
    }

    #[test]
    fn drain_due_returns_first_event_at_one_second() {
        let started = Instant::now();
        let mut demo = DemoState::new_at(started);
        let events = demo.drain_due(started + Duration::from_secs(1));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].repo_full_name, "rust-lang/rust");
        assert_eq!(events[0].kind, EventKind::PrOpened);
    }

    #[test]
    fn drain_due_does_not_re_emit_events() {
        // After draining, a subsequent call at the same or earlier
        // time must return no events for the same cursor.
        let started = Instant::now();
        let mut demo = DemoState::new_at(started);
        let _ = demo.drain_due(started + Duration::from_secs(2));
        // First two events have fired; further calls at the same
        // time return nothing.
        assert!(demo.drain_due(started + Duration::from_secs(2)).is_empty());
    }

    #[test]
    fn remaining_secs_counts_down_to_zero() {
        let started = Instant::now();
        let demo = DemoState::new_at(started);
        // At start: full window.
        assert_eq!(demo.remaining_secs(started), DEMO_TOTAL_SECS);
        // After 10s: 110s left.
        assert_eq!(
            demo.remaining_secs(started + Duration::from_secs(10)),
            DEMO_TOTAL_SECS - 10
        );
        // After DEMO_TOTAL_SECS: 0.
        assert_eq!(
            demo.remaining_secs(started + Duration::from_secs(DEMO_TOTAL_SECS)),
            0
        );
        // Past the window: still 0 (saturating).
        assert_eq!(
            demo.remaining_secs(started + Duration::from_secs(DEMO_TOTAL_SECS + 100)),
            0
        );
    }

    #[test]
    fn is_complete_flips_at_total_secs() {
        let started = Instant::now();
        let demo = DemoState::new_at(started);
        assert!(!demo.is_complete(started));
        assert!(!demo.is_complete(started + Duration::from_secs(10)));
        assert!(!demo.is_complete(started + Duration::from_secs(DEMO_TOTAL_SECS - 1)));
        assert!(demo.is_complete(started + Duration::from_secs(DEMO_TOTAL_SECS)));
    }
}
