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
    #[error("rate-limited by GitHub")]
    RateLimited,
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
            warn!("GitHub rate-limited us");
            return Err(ClientError::RateLimited);
        }
        if status == reqwest::StatusCode::UNAUTHORIZED
            || status == reqwest::StatusCode::FORBIDDEN
        {
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

        let client = Client::new(test_auth(), ClientConfig {
            base_url: server.uri(),
            user_agent: "test".to_string(),
            timeout: Duration::from_secs(5),
        })
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
        let client = Client::new(test_auth(), ClientConfig {
            base_url: server.uri(),
            user_agent: "test".to_string(),
            timeout: Duration::from_secs(5),
        })
        .unwrap();
        let r = client.received_events("u").await;
        assert!(matches!(r, Err(ClientError::RateLimited)));
    }

    #[tokio::test]
    async fn unauthorized_is_handled() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let client = Client::new(test_auth(), ClientConfig {
            base_url: server.uri(),
            user_agent: "test".to_string(),
            timeout: Duration::from_secs(5),
        })
        .unwrap();
        let r = client.received_events("u").await;
        assert!(matches!(r, Err(ClientError::Unauthorized(_))));
    }
}
