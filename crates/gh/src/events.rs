//! GitHub event types: parsing the REST `events` API.
//!
//! Reference: <https://docs.github.com/en/rest/activity/events>.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The five event kinds we surface in v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EventKind {
    /// A pull request was opened.
    PrOpened,
    /// A pull request was merged.
    PrMerged,
    /// An issue was opened.
    IssueOpened,
    /// A release was published.
    ReleasePublished,
    /// A new repository was created.
    RepoCreated,
}

impl EventKind {
    /// Whether this event is rare and important and should never be
    /// compressed into a (type, count) pair.
    pub fn is_standalone(&self) -> bool {
        matches!(self, EventKind::RepoCreated)
    }
}

/// A raw GitHub event from the REST API, normalized to the five kinds we
/// care about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawEvent {
    /// GitHub's global event id (string, may be numeric).
    pub id: String,
    /// Event kind.
    pub kind: EventKind,
    /// "owner/name"
    pub repo_full_name: String,
    /// When the event was created.
    pub created_at: DateTime<Utc>,
    /// Optional title (PR title, issue title, release tag/name).
    pub title: Option<String>,
    /// Optional URL to the event in the GitHub web UI.
    pub url: Option<url::Url>,
}

impl RawEvent {
    /// Helper used only in tests and seed data.
    #[doc(hidden)]
    pub fn for_test(repo_full_name: String, kind: EventKind, created_at: DateTime<Utc>) -> Self {
        Self {
            id: format!(
                "test-{}",
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
            ),
            kind,
            repo_full_name,
            created_at,
            title: None,
            url: None,
        }
    }

    /// The "owner/name" of the repo.
    pub fn repo_full_name(&self) -> String {
        self.repo_full_name.clone()
    }
}

/// Parse a list of GitHub events from a JSON body. Individual events with
/// unsupported types or actions are skipped (logged via `tracing::debug`)
/// so one PushEvent in the batch doesn't drop every other event. Structural
/// errors (bad JSON, missing required fields) still propagate.
pub fn parse_events(body: &str) -> Result<Vec<RawEvent>, ParseError> {
    let raw: Vec<serde_json::Value> = serde_json::from_str(body)?;
    let mut out = Vec::with_capacity(raw.len());
    for value in raw {
        match parse_event(value) {
            Ok(ev) => out.push(ev),
            Err(ParseError::UnsupportedEvent(t)) => {
                tracing::debug!(event_type = %t, "skipping unsupported event");
            }
            Err(ParseError::UnsupportedAction(a)) => {
                tracing::debug!(action = %a, "skipping unsupported action");
            }
            Err(other) => return Err(other),
        }
    }
    Ok(out)
}

/// Parse a single GitHub event.
pub fn parse_event(value: serde_json::Value) -> Result<RawEvent, ParseError> {
    let id = value
        .get("id")
        .and_then(|v| match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .ok_or_else(|| ParseError::MissingField("id".to_string()))?;

    let event_type = value
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ParseError::MissingField("type".to_string()))?;

    let created_at = value
        .get("created_at")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
        .ok_or_else(|| ParseError::MissingField("created_at".to_string()))?;

    let repo_full_name = value
        .get("repo")
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| ParseError::MissingField("repo.name".to_string()))?
        .to_string();

    let payload = value.get("payload");

    let (kind, title, url) = match event_type {
        "PullRequestEvent" => {
            let action = payload
                .and_then(|p| p.get("action"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let title = payload
                .and_then(|p| p.get("pull_request"))
                .and_then(|pr| pr.get("title"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let url = payload
                .and_then(|p| p.get("pull_request"))
                .and_then(|pr| pr.get("html_url"))
                .and_then(|v| v.as_str())
                .and_then(|s| url::Url::parse(s).ok());
            match action {
                "opened" => (EventKind::PrOpened, title, url),
                "closed" => {
                    let merged = payload
                        .and_then(|p| p.get("pull_request"))
                        .and_then(|pr| pr.get("merged"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if merged {
                        (EventKind::PrMerged, title, url)
                    } else {
                        return Err(ParseError::UnsupportedAction(format!(
                            "PullRequestEvent:{}",
                            action
                        )));
                    }
                }
                other => {
                    return Err(ParseError::UnsupportedAction(format!(
                        "PullRequestEvent:{}",
                        other
                    )));
                }
            }
        }
        "IssuesEvent" => {
            let action = payload
                .and_then(|p| p.get("action"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let title = payload
                .and_then(|p| p.get("issue"))
                .and_then(|i| i.get("title"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let url = payload
                .and_then(|p| p.get("issue"))
                .and_then(|i| i.get("html_url"))
                .and_then(|v| v.as_str())
                .and_then(|s| url::Url::parse(s).ok());
            if action != "opened" {
                return Err(ParseError::UnsupportedAction(format!(
                    "IssuesEvent:{}",
                    action
                )));
            }
            (EventKind::IssueOpened, title, url)
        }
        "ReleaseEvent" => {
            let action = payload
                .and_then(|p| p.get("action"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let title = payload
                .and_then(|p| p.get("release"))
                .and_then(|r| r.get("name"))
                .and_then(|v| v.as_str())
                .or_else(|| {
                    payload
                        .and_then(|p| p.get("release"))
                        .and_then(|r| r.get("tag_name"))
                        .and_then(|v| v.as_str())
                })
                .map(|s| s.to_string());
            let url = payload
                .and_then(|p| p.get("release"))
                .and_then(|r| r.get("html_url"))
                .and_then(|v| v.as_str())
                .and_then(|s| url::Url::parse(s).ok());
            if action != "published" {
                return Err(ParseError::UnsupportedAction(format!(
                    "ReleaseEvent:{}",
                    action
                )));
            }
            (EventKind::ReleasePublished, title, url)
        }
        "CreateEvent" => {
            // CreateEvent: ref_type is "repository" when a new repo is created
            let ref_type = payload
                .and_then(|p| p.get("ref_type"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if ref_type != "repository" {
                return Err(ParseError::UnsupportedAction(format!(
                    "CreateEvent:{}",
                    ref_type
                )));
            }
            // No specific URL in the payload for repo-created events;
            // use the repo page. The caller can still pass a richer URL
            // by constructing the RawEvent directly.
            (EventKind::RepoCreated, None, None)
        }
        other => {
            return Err(ParseError::UnsupportedEvent(other.to_string()));
        }
    };

    Ok(RawEvent {
        id,
        kind,
        repo_full_name,
        created_at,
        title,
        url,
    })
}

/// Errors from parsing GitHub events.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("missing required field: {0}")]
    MissingField(String),
    #[error("unsupported event type: {0}")]
    UnsupportedEvent(String),
    #[error("unsupported action: {0}")]
    UnsupportedAction(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pr_opened() {
        let json = r#"{
            "id": "1",
            "type": "PullRequestEvent",
            "created_at": "2026-06-13T10:00:00Z",
            "repo": {"name": "octocat/Hello-World"},
            "payload": {
                "action": "opened",
                "pull_request": {
                    "title": "Fix typo",
                    "merged": false,
                    "html_url": "https://github.com/octocat/Hello-World/pull/42"
                }
            }
        }"#;
        let ev = parse_event(serde_json::from_str(json).unwrap()).unwrap();
        assert_eq!(ev.kind, EventKind::PrOpened);
        assert_eq!(ev.repo_full_name, "octocat/Hello-World");
        assert_eq!(ev.title.as_deref(), Some("Fix typo"));
        assert_eq!(
            ev.url.as_ref().map(|u| u.to_string()),
            Some("https://github.com/octocat/Hello-World/pull/42".to_string())
        );
    }

    #[test]
    fn parses_pr_merged() {
        let json = r#"{
            "id": "2",
            "type": "PullRequestEvent",
            "created_at": "2026-06-13T10:00:00Z",
            "repo": {"name": "x/y"},
            "payload": {
                "action": "closed",
                "pull_request": {"title": "X", "merged": true}
            }
        }"#;
        let ev = parse_event(serde_json::from_str(json).unwrap()).unwrap();
        assert_eq!(ev.kind, EventKind::PrMerged);
    }

    #[test]
    fn parses_pr_closed_unmerged_is_error() {
        let json = r#"{
            "id": "3",
            "type": "PullRequestEvent",
            "created_at": "2026-06-13T10:00:00Z",
            "repo": {"name": "x/y"},
            "payload": {
                "action": "closed",
                "pull_request": {"title": "X", "merged": false}
            }
        }"#;
        assert!(parse_event(serde_json::from_str(json).unwrap()).is_err());
    }

    #[test]
    fn parses_issue_opened() {
        let json = r#"{
            "id": "4",
            "type": "IssuesEvent",
            "created_at": "2026-06-13T10:00:00Z",
            "repo": {"name": "x/y"},
            "payload": {
                "action": "opened",
                "issue": {
                    "title": "Bug",
                    "html_url": "https://github.com/x/y/issues/7"
                }
            }
        }"#;
        let ev = parse_event(serde_json::from_str(json).unwrap()).unwrap();
        assert_eq!(ev.kind, EventKind::IssueOpened);
        assert_eq!(
            ev.url.as_ref().map(|u| u.to_string()),
            Some("https://github.com/x/y/issues/7".to_string())
        );
    }

    #[test]
    fn parses_release_published() {
        let json = r#"{
            "id": "5",
            "type": "ReleaseEvent",
            "created_at": "2026-06-13T10:00:00Z",
            "repo": {"name": "x/y"},
            "payload": {
                "action": "published",
                "release": {
                    "name": "v1.0",
                    "tag_name": "v1.0",
                    "html_url": "https://github.com/x/y/releases/tag/v1.0"
                }
            }
        }"#;
        let ev = parse_event(serde_json::from_str(json).unwrap()).unwrap();
        assert_eq!(ev.kind, EventKind::ReleasePublished);
        assert_eq!(
            ev.url.as_ref().map(|u| u.to_string()),
            Some("https://github.com/x/y/releases/tag/v1.0".to_string())
        );
    }

    #[test]
    fn parses_repo_created() {
        let json = r#"{
            "id": "6",
            "type": "CreateEvent",
            "created_at": "2026-06-13T10:00:00Z",
            "repo": {"name": "x/y"},
            "payload": {"ref_type": "repository"}
        }"#;
        let ev = parse_event(serde_json::from_str(json).unwrap()).unwrap();
        assert_eq!(ev.kind, EventKind::RepoCreated);
        assert!(ev.kind.is_standalone());
    }

    #[test]
    fn unsupported_event_is_error_not_panic() {
        let json = r#"{
            "id": "7",
            "type": "PushEvent",
            "created_at": "2026-06-13T10:00:00Z",
            "repo": {"name": "x/y"},
            "payload": {}
        }"#;
        assert!(matches!(
            parse_event(serde_json::from_str(json).unwrap()),
            Err(ParseError::UnsupportedEvent(_))
        ));
    }

    #[test]
    fn parse_list_filters_unsupported() {
        let body = r#"[
            {
                "id": "1",
                "type": "PushEvent",
                "created_at": "2026-06-13T10:00:00Z",
                "repo": {"name": "x/y"},
                "payload": {}
            },
            {
                "id": "2",
                "type": "IssuesEvent",
                "created_at": "2026-06-13T10:00:00Z",
                "repo": {"name": "x/y"},
                "payload": {"action": "opened", "issue": {"title": "Bug"}}
            },
            {
                "id": "3",
                "type": "PullRequestEvent",
                "created_at": "2026-06-13T10:00:00Z",
                "repo": {"name": "x/y"},
                "payload": {"action": "closed", "pull_request": {"title": "X", "merged": false}}
            }
        ]"#;
        let res = parse_events(body).unwrap();
        assert_eq!(res.len(), 1, "PushEvent + unmerged PR closed are filtered");
        assert_eq!(res[0].id, "2");
        assert_eq!(res[0].kind, EventKind::IssueOpened);
    }
}
