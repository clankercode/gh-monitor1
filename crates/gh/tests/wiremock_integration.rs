//! Wiremock-based integration tests for the GitHub client.

use std::time::Duration;

use gh_monitor_gh::client::{Client, ClientConfig};
use gh_monitor_gh::{Auth, EventKind};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_auth() -> Auth {
    Auth::new("ghp_test123").unwrap()
}

fn test_cfg(server: &MockServer) -> ClientConfig {
    ClientConfig {
        base_url: server.uri(),
        user_agent: "gh-monitor-test".to_string(),
        timeout: Duration::from_secs(5),
    }
}

const PR_OPENED_BODY: &str = r#"[
    {
        "id": "1001",
        "type": "PullRequestEvent",
        "created_at": "2026-06-13T10:00:00Z",
        "repo": {"name": "octocat/Hello-World"},
        "payload": {
            "action": "opened",
            "pull_request": {"title": "Add support for X", "merged": false}
        }
    }
]"#;

const ISSUES_BODY: &str = r#"[
    {
        "id": "1002",
        "type": "IssuesEvent",
        "created_at": "2026-06-13T10:01:00Z",
        "repo": {"name": "octocat/Hello-World"},
        "payload": {
            "action": "opened",
            "issue": {"title": "Bug report"}
        }
    }
]"#;

#[tokio::test]
async fn received_events_full_pipeline() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/octocat/received_events"))
        .and(header("Authorization", "Bearer ghp_test123"))
        .respond_with(ResponseTemplate::new(200).set_body_string(PR_OPENED_BODY))
        .mount(&server)
        .await;

    let client = Client::new(test_auth(), test_cfg(&server)).unwrap();
    let events = client.received_events("octocat").await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EventKind::PrOpened);
    assert_eq!(events[0].repo_full_name, "octocat/Hello-World");
    assert_eq!(events[0].title.as_deref(), Some("Add support for X"));
}

#[tokio::test]
async fn org_events_handles_empty() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/orgs/rust-lang/events"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server)
        .await;
    let client = Client::new(test_auth(), test_cfg(&server)).unwrap();
    let events = client.org_events("rust-lang").await.unwrap();
    assert!(events.is_empty());
}

#[tokio::test]
async fn multiple_endpoints_in_sequence() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/octocat/received_events"))
        .respond_with(ResponseTemplate::new(200).set_body_string(PR_OPENED_BODY))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/orgs/rust-lang/events"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ISSUES_BODY))
        .mount(&server)
        .await;
    let client = Client::new(test_auth(), test_cfg(&server)).unwrap();
    let prs = client.received_events("octocat").await.unwrap();
    let issues = client.org_events("rust-lang").await.unwrap();
    assert_eq!(prs.len(), 1);
    assert_eq!(issues.len(), 1);
    assert_eq!(prs[0].kind, EventKind::PrOpened);
    assert_eq!(issues[0].kind, EventKind::IssueOpened);
}
