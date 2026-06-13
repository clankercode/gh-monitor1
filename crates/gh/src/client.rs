//! The HTTP client that talks to the GitHub REST API.

use std::time::Duration;

use reqwest::Client as ReqwestClient;
use thiserror::Error;
use tracing::{debug, warn};

use crate::auth::Auth;
use crate::events::{parse_events, RawEvent};

const DEFAULT_BASE: &str = "https://api.github.com";

/// Configuration for the [`Client`].
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// API base URL. Override for GitHub Enterprise.
    pub base_url: String,
    /// User-Agent header value.
    pub user_agent: String,
    /// Request timeout.
    pub timeout: Duration,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE.to_string(),
            user_agent: format!("gh-monitor/{}", env!("CARGO_PKG_VERSION")),
            timeout: Duration::from_secs(30),
        }
    }
}

/// Errors from the GitHub client.
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    /// `429 Too Many Requests`. `reset_at` carries the absolute Unix
    /// epoch seconds when the rate-limit window resets, parsed from
    /// the `X-RateLimit-Reset` header (or, as a fallback, the
    /// `Retry-After` delta in seconds added to "now"). `None` means
    /// the server didn't supply either header. The poller formats the
    /// timestamp into a "rate-limited until HH:MM:SS" string for the
    /// status banner.
    #[error("rate-limited by GitHub until {reset_at:?}")]
    RateLimited { reset_at: Option<u64> },
    #[error("auth error: {0}")]
    Unauthorized(String),
    #[error("server error: {0}")]
    Server(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("events API: {0}")]
    Events(String),
}

/// The HTTP client. Cheap to clone (uses an `Arc` internally via reqwest).
#[derive(Debug, Clone)]
pub struct Client {
    inner: ReqwestClient,
    auth: Auth,
    cfg: ClientConfig,
}

impl Client {
    /// Build a new client. The reqwest client is shared across all calls.
    pub fn new(auth: Auth, cfg: ClientConfig) -> Result<Self, ClientError> {
        let inner = ReqwestClient::builder()
            .user_agent(cfg.user_agent.clone())
            .timeout(cfg.timeout)
            .build()?;
        Ok(Self { inner, auth, cfg })
    }

    /// Get events for the authenticated user (`/users/{user}/received_events`).
    pub async fn received_events(&self, username: &str) -> Result<Vec<RawEvent>, ClientError> {
        let url = format!("{}/users/{}/received_events", self.cfg.base_url, username);
        self.get_events(&url).await
    }

    /// Get events for an organization (`/orgs/{org}/events`).
    pub async fn org_events(&self, org: &str) -> Result<Vec<RawEvent>, ClientError> {
        let url = format!("{}/orgs/{}/events", self.cfg.base_url, org);
        self.get_events(&url).await
    }

    /// Get events for a single repo (`/repos/{owner}/{repo}/events`).
    pub async fn repo_events(&self, owner: &str, repo: &str) -> Result<Vec<RawEvent>, ClientError> {
        let url = format!("{}/repos/{}/{}/events", self.cfg.base_url, owner, repo);
        self.get_events(&url).await
    }

    async fn get_events(&self, url: &str) -> Result<Vec<RawEvent>, ClientError> {
        debug!(url = %url, "GET events");
        let resp = self
            .inner
            .get(url)
            .header("Authorization", self.auth.header_value())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await?;

        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let reset_at = parse_rate_limit_reset(resp.headers());
            warn!(reset_at = ?reset_at, "GitHub rate-limited us");
            return Err(ClientError::RateLimited { reset_at });
        }
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ClientError::Unauthorized(status.to_string()));
        }
        if status.is_server_error() {
            return Err(ClientError::Server(status.to_string()));
        }
        if !status.is_success() {
            return Err(ClientError::Events(status.to_string()));
        }

        let body = resp.text().await?;
        parse_events(&body).map_err(|e| ClientError::Parse(e.to_string()))
    }
}

/// Extract a Unix-epoch-seconds "rate limit resets at" timestamp from
/// the response headers. We try, in order:
///
/// 1. `X-RateLimit-Reset` — an absolute Unix epoch in seconds. This
///    is the canonical GitHub hint and the one the user-facing banner
///    should display.
/// 2. `Retry-After` — either an integer number of seconds or an
///    HTTP-date. We only honour the integer form (it's what GitHub
///    sends today) and convert it to an absolute timestamp relative
///    to "now" using the system clock.
///
/// Returns `None` if neither header is present or both fail to parse.
fn parse_rate_limit_reset(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    if let Some(v) = headers
        .get("x-ratelimit-reset")
        .and_then(|h| h.to_str().ok())
    {
        if let Ok(n) = v.trim().parse::<u64>() {
            return Some(n);
        }
    }
    if let Some(v) = headers.get("retry-after").and_then(|h| h.to_str().ok()) {
        if let Ok(secs) = v.trim().parse::<u64>() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            return Some(now.saturating_add(secs));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_auth() -> Auth {
        Auth::new("ghp_test").unwrap()
    }

    #[tokio::test]
    async fn received_events_parses_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/users/octocat/received_events"))
            .and(header("Authorization", "Bearer ghp_test"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"[
                    {
                        "id": "1",
                        "type": "IssuesEvent",
                        "created_at": "2026-06-13T10:00:00Z",
                        "repo": {"name": "x/y"},
                        "payload": {"action": "opened", "issue": {"title": "Bug"}}
                    }
                ]"#,
            ))
            .mount(&server)
            .await;

        let client = Client::new(
            test_auth(),
            ClientConfig {
                base_url: server.uri(),
                user_agent: "test".to_string(),
                timeout: Duration::from_secs(5),
            },
        )
        .unwrap();
        let events = client.received_events("octocat").await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, crate::EventKind::IssueOpened);
    }

    #[tokio::test]
    async fn rate_limit_is_handled() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let client = Client::new(
            test_auth(),
            ClientConfig {
                base_url: server.uri(),
                user_agent: "test".to_string(),
                timeout: Duration::from_secs(5),
            },
        )
        .unwrap();
        let r = client.received_events("u").await;
        // No `X-RateLimit-Reset` or `Retry-After` header on the mock —
        // the variant is still surfaced with `reset_at: None`.
        assert!(matches!(
            r,
            Err(ClientError::RateLimited { reset_at: None })
        ));
    }

    #[tokio::test]
    async fn rate_limit_with_x_ratelimit_reset_header() {
        // The GitHub API sends an absolute Unix-epoch timestamp in
        // `X-RateLimit-Reset` on 429. We must surface it on the
        // error so the poller can show "rate-limited until HH:MM:SS"
        // on the status banner.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(429).insert_header("X-RateLimit-Reset", "1234567890"),
            )
            .mount(&server)
            .await;
        let client = Client::new(
            test_auth(),
            ClientConfig {
                base_url: server.uri(),
                user_agent: "test".to_string(),
                timeout: Duration::from_secs(5),
            },
        )
        .unwrap();
        let r = client.received_events("u").await.unwrap_err();
        match r {
            ClientError::RateLimited { reset_at: Some(ts) } => assert_eq!(ts, 1234567890),
            other => panic!("expected RateLimited with reset, got {other:?}"),
        }
        let msg = format!("{}", r);
        assert!(
            msg.contains("rate-limited"),
            "error message should include 'rate-limited', got: {msg}"
        );
    }

    #[tokio::test]
    async fn rate_limit_with_retry_after_falls_back() {
        // When `X-RateLimit-Reset` is absent but `Retry-After: <secs>`
        // is present, the parser should compute an absolute timestamp
        // by adding the seconds to the current system clock.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "60"))
            .mount(&server)
            .await;
        let client = Client::new(
            test_auth(),
            ClientConfig {
                base_url: server.uri(),
                user_agent: "test".to_string(),
                timeout: Duration::from_secs(5),
            },
        )
        .unwrap();
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let r = client.received_events("u").await.unwrap_err();
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        match r {
            ClientError::RateLimited { reset_at: Some(ts) } => {
                assert!(
                    ts >= before + 60 && ts <= after + 60,
                    "expected ~now+60s ({before}+60..={after}+60), got {ts}"
                );
            }
            other => panic!("expected RateLimited with reset, got {other:?}"),
        }
    }

    #[test]
    fn parse_rate_limit_reset_prefers_x_header() {
        // When both headers are present, `X-RateLimit-Reset` wins
        // (it's an absolute timestamp; `Retry-After` is relative and
        // would require clock-skew handling).
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-ratelimit-reset", "1700000000".parse().unwrap());
        headers.insert("retry-after", "30".parse().unwrap());
        assert_eq!(parse_rate_limit_reset(&headers), Some(1700000000));
    }

    #[test]
    fn parse_rate_limit_reset_handles_garbage() {
        // Garbage in either header is treated as "no reset info".
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-ratelimit-reset", "not a number".parse().unwrap());
        assert_eq!(parse_rate_limit_reset(&headers), None);
    }

    #[tokio::test]
    async fn unauthorized_is_handled() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let client = Client::new(
            test_auth(),
            ClientConfig {
                base_url: server.uri(),
                user_agent: "test".to_string(),
                timeout: Duration::from_secs(5),
            },
        )
        .unwrap();
        let r = client.received_events("u").await;
        assert!(matches!(r, Err(ClientError::Unauthorized(_))));
    }
}
