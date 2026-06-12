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
        HumanRange::Point(ago_label(latest, now))
    } else {
        let e_ago = humanize_one(earliest, now);
        let l_ago = humanize_one(latest, now);
        if e_ago == l_ago {
            HumanRange::Point(e_ago)
        } else {
            HumanRange::Range(format!("{}-{} ago", e_ago, l_ago))
        }
    }
}

fn ago_label(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    humanize_one(t, now)
}

fn humanize_one(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let delta = now - t;
    let secs = delta.num_seconds();

    if secs < 0 {
        return format_in(secs.unsigned_abs());
    }

    if secs < 5 {
        return "just now".to_string();
    }
    if secs < 60 {
        return format!("{} secs ago", secs);
    }
    let mins = secs / 60;
    if mins < 60 {
        return format_unit(mins, "min");
    }
    let hrs = mins / 60;
    if hrs < 24 {
        return format_unit(hrs, "hr");
    }
    let days = hrs / 24;
    if days < 7 {
        return format_unit(days, "day");
    }
    let weeks = days / 7;
    if weeks < 5 {
        return format_unit(weeks, "wk");
    }
    let months = days / 30;
    if months < 12 {
        return format_unit(months, "mo");
    }
    let years = days / 365;
    format_unit(years, "yr")
}

fn format_unit(n: i64, unit: &str) -> String {
    let s = if n == 1 { "" } else { "s" };
    format!("{} {}{} ago", n, unit, s)
}

fn format_in(secs: u64) -> String {
    if secs < 60 {
        return format!("in {} secs", secs);
    }
    let mins = secs / 60;
    if mins < 60 {
        return format_unit_future(mins, "min");
    }
    let hrs = mins / 60;
    if hrs < 24 {
        return format_unit_future(hrs, "hr");
    }
    let days = hrs / 24;
    format_unit_future(days, "day")
}

fn format_unit_future(n: u64, unit: &str) -> String {
    let s = if n == 1 { "" } else { "s" };
    format!("in {} {}{}", n, unit, s)
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
}
