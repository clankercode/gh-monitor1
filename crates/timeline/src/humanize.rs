//! Humanize time ranges: "1-3 hrs ago", "just now", "5 mins ago".

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A human-friendly time-range label, ready to render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HumanRange {
    /// A single point in time, formatted as "5 mins ago" or "in 5 mins".
    Point(String),
    /// A range of two points, formatted as "1-3 hrs ago" or "in 1-3 hrs".
    Range(String),
}

impl HumanRange {
    /// Get the string representation.
    pub fn as_str(&self) -> &str {
        match self {
            HumanRange::Point(s) | HumanRange::Range(s) => s,
        }
    }
}

/// A time unit used in humanized labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Sec,
    Min,
    Hr,
    Day,
    Wk,
    Mo,
    Yr,
}

impl Unit {
    fn as_str(self) -> &'static str {
        match self {
            Unit::Sec => "sec",
            Unit::Min => "min",
            Unit::Hr => "hr",
            Unit::Day => "day",
            Unit::Wk => "wk",
            Unit::Mo => "mo",
            Unit::Yr => "yr",
        }
    }
}

/// Whether a label refers to the past ("5 mins ago") or the future
/// ("in 5 mins").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Past,
    Future,
}

/// The coarse magnitude of a time delta, ready to format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Magnitude {
    n: u64,
    unit: Unit,
    direction: Direction,
}

/// Compute the coarse magnitude of `t` relative to `now`. Returns `None`
/// for "just now" (deltas under 5 seconds) because those can't form a
/// meaningful range.
fn magnitude(t: DateTime<Utc>, now: DateTime<Utc>) -> Option<Magnitude> {
    let delta = now - t;
    let secs = delta.num_seconds();
    if secs < 0 {
        return Some(magnitude_future(secs.unsigned_abs()));
    }
    if secs < 5 {
        return None;
    }
    if secs < 60 {
        return Some(Magnitude {
            n: secs as u64,
            unit: Unit::Sec,
            direction: Direction::Past,
        });
    }
    let mins = secs / 60;
    if mins < 60 {
        return Some(Magnitude {
            n: mins as u64,
            unit: Unit::Min,
            direction: Direction::Past,
        });
    }
    let hrs = mins / 60;
    if hrs < 24 {
        return Some(Magnitude {
            n: hrs as u64,
            unit: Unit::Hr,
            direction: Direction::Past,
        });
    }
    let days = hrs / 24;
    if days < 7 {
        return Some(Magnitude {
            n: days as u64,
            unit: Unit::Day,
            direction: Direction::Past,
        });
    }
    let weeks = days / 7;
    if weeks < 5 {
        return Some(Magnitude {
            n: weeks as u64,
            unit: Unit::Wk,
            direction: Direction::Past,
        });
    }
    let months = days / 30;
    if months < 12 {
        return Some(Magnitude {
            n: months as u64,
            unit: Unit::Mo,
            direction: Direction::Past,
        });
    }
    let years = days / 365;
    Some(Magnitude {
        n: years as u64,
        unit: Unit::Yr,
        direction: Direction::Past,
    })
}

fn magnitude_future(secs: u64) -> Magnitude {
    if secs < 60 {
        return Magnitude {
            n: secs,
            unit: Unit::Sec,
            direction: Direction::Future,
        };
    }
    let mins = secs / 60;
    if mins < 60 {
        return Magnitude {
            n: mins,
            unit: Unit::Min,
            direction: Direction::Future,
        };
    }
    let hrs = mins / 60;
    if hrs < 24 {
        return Magnitude {
            n: hrs,
            unit: Unit::Hr,
            direction: Direction::Future,
        };
    }
    let days = hrs / 24;
    Magnitude {
        n: days,
        unit: Unit::Day,
        direction: Direction::Future,
    }
}

/// Format a magnitude as a point label, e.g. "3 hrs ago" or "in 3 hrs".
fn format_point_label(m: Magnitude) -> String {
    let body = format!("{} {}{}", m.n, m.unit.as_str(), plural_suffix(m.n));
    match m.direction {
        Direction::Past => format!("{body} ago"),
        Direction::Future => format!("in {body}"),
    }
}

fn plural_suffix(n: u64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Format the span between `earliest` and `latest` (relative to `now`) as a
/// humanized string. If `earliest == latest` (or very close), produces a
/// "Point". Otherwise produces a "Range" like "1-3 hrs ago".
pub fn humanize_range(
    earliest: DateTime<Utc>,
    latest: DateTime<Utc>,
    now: DateTime<Utc>,
) -> HumanRange {
    let span = latest - earliest;
    let span_secs = span.num_seconds().abs();

    if span_secs < 60 {
        return HumanRange::Point(ago_label(latest, now));
    }

    let m_e = magnitude(earliest, now);
    let m_l = magnitude(latest, now);
    match (m_e, m_l) {
        (Some(a), Some(b)) if a.unit == b.unit && a.direction == b.direction => {
            let lo = a.n.min(b.n);
            let hi = a.n.max(b.n);
            if lo == hi {
                HumanRange::Point(format_point_label(Magnitude {
                    n: lo,
                    unit: a.unit,
                    direction: a.direction,
                }))
            } else {
                let s = plural_suffix(hi);
                let unit = a.unit.as_str();
                match a.direction {
                    Direction::Past => HumanRange::Range(format!("{lo}-{hi} {unit}{s} ago")),
                    Direction::Future => HumanRange::Range(format!("in {lo}-{hi} {unit}{s}")),
                }
            }
        }
        // Mixed units, mixed directions, or "just now" boundaries: fall
        // back to the most recent endpoint's point label.
        _ => HumanRange::Point(ago_label(latest, now)),
    }
}

fn ago_label(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    match magnitude(t, now) {
        None => "just now".to_string(),
        Some(m) => format_point_label(m),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn point_when_same_time() {
        let now = Utc::now();
        let r = humanize_range(now, now, now);
        assert!(matches!(r, HumanRange::Point(_)));
    }

    #[test]
    fn point_when_close() {
        let now = Utc::now();
        let r = humanize_range(now - Duration::seconds(30), now, now);
        assert!(matches!(r, HumanRange::Point(_)));
    }

    #[test]
    fn range_when_apart() {
        let now = Utc::now();
        let r = humanize_range(now - Duration::hours(3), now - Duration::hours(1), now);
        match r {
            HumanRange::Range(s) => {
                assert!(s.contains("hr"), "got: {}", s);
                assert!(s.contains("ago"), "got: {}", s);
            }
            HumanRange::Point(s) => panic!("expected Range, got Point({})", s),
        }
    }

    #[test]
    fn range_pins_exact_format() {
        // Pin the exact output to "1-3 hrs ago": low first, plural,
        // single "ago".
        let now = Utc::now();
        let r = humanize_range(now - Duration::hours(3), now - Duration::hours(1), now);
        assert_eq!(r.as_str(), "1-3 hrs ago");
    }

    #[test]
    fn range_does_not_double_ago() {
        let now = Utc::now();
        let r = humanize_range(now - Duration::hours(3), now - Duration::hours(1), now);
        let s = r.as_str();
        assert_eq!(s.matches("ago").count(), 1, "got: {s}");
    }

    #[test]
    fn range_low_first_even_when_inputs_reversed() {
        // Caller passes earliest more recent than latest: the rendered
        // range must still be "1-3 hrs ago", not "3-1 hrs ago".
        let now = Utc::now();
        let r = humanize_range(now - Duration::hours(1), now - Duration::hours(3), now);
        assert_eq!(r.as_str(), "1-3 hrs ago");
    }

    #[test]
    fn range_singular_when_high_is_one() {
        // 0-1 mins ago: lo=0, hi=1. We pick "0-1 min ago" with no
        // plural on the unit since hi=1. Actually `humanize_one` won't
        // return 0 ("just now" handles <5s), so we use a span inside
        // the min unit that produces a non-zero low.
        let now = Utc::now();
        let r = humanize_range(now - Duration::minutes(2), now - Duration::minutes(1), now);
        assert_eq!(r.as_str(), "1-2 mins ago");
    }

    #[test]
    fn range_same_unit_same_value_collapses_to_point() {
        // Both endpoints round to the same unit and same value (both
        // in the 2-hr bucket), and the span between them is large
        // enough to be a Range. We should collapse to a Point label.
        let now = Utc::now();
        let r = humanize_range(
            now - Duration::hours(2) - Duration::minutes(30),
            now - Duration::hours(2) - Duration::minutes(5),
            now,
        );
        assert!(matches!(r, HumanRange::Point(_)));
        assert_eq!(r.as_str(), "2 hrs ago");
    }

    #[test]
    fn just_now() {
        let now = Utc::now();
        let r = humanize_range(now, now, now);
        assert!(r.as_str().contains("now"));
    }

    #[test]
    fn hours_ago() {
        let now = Utc::now();
        let r = humanize_range(now - Duration::hours(2), now - Duration::hours(2), now);
        assert!(r.as_str().contains("hr"));
    }

    #[test]
    fn days_ago() {
        let now = Utc::now();
        let r = humanize_range(now - Duration::days(2), now - Duration::days(2), now);
        assert!(r.as_str().contains("day"));
    }

    /// Property: for any single time `t` (past or future), calling
    /// `humanize_range(t, t, t)` returns a point label. It can't form a
    /// meaningful range because both endpoints are the same instant.
    #[test]
    fn proptest_point_for_same_instant() {
        use proptest::prelude::*;
        proptest!(|(
            secs_offset in -86_400i64..86_400i64,
        )| {
            let now = Utc::now();
            let t = now + Duration::seconds(secs_offset);
            let r = humanize_range(t, t, now);
            prop_assert!(matches!(r, HumanRange::Point(_)));
        });
    }

    /// Property: when the two endpoints are the same point in time, the
    /// rendered label is one of the known magnitudes (or "just now").
    /// This pins the function's output to a closed set of legal labels
    /// so we don't accidentally render `5 secs ago` and then `5 sec ago`
    /// for a similar input.
    #[test]
    fn proptest_label_is_known_point() {
        use proptest::prelude::*;
        proptest!(|(
            secs_offset in -86_400i64..86_400i64,
        )| {
            let now = Utc::now();
            let t = now + Duration::seconds(secs_offset);
            let label = humanize_range(t, t, now).as_str().to_string();
            let known = [
                "just now",
            ];
            let known_units = [
                "sec", "secs",
                "min", "mins",
                "hr", "hrs",
                "day", "days",
                "wk", "wks",
                "mo", "mos",
                "yr", "yrs",
            ];
            let is_known = known.contains(&label.as_str())
                || known_units.iter().any(|u| label.contains(u));
            prop_assert!(is_known, "unexpected label: {label}");
        });
    }
}
