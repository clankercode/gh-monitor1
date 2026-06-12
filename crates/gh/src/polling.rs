//! The polling loop: repeatedly fetch events from all sources and emit them
//! on a tokio channel.

use std::time::Duration;

use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::auth::Auth;
use crate::client::{Client, ClientConfig, ClientError};
use crate::events::RawEvent;

/// What to poll.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PollConfig {
    /// Username to fetch `received_events` for. If `None`, skip.
    pub username: Option<String>,
    /// Orgs to fetch `org_events` for.
    pub orgs: Vec<String>,
    /// Specific repos to fetch events for ("owner/name").
    pub repos: Vec<String>,
    /// Poll interval.
    pub interval: Duration,
}

impl PollConfig {
    /// A new config with default 30s interval.
    pub fn with_default_interval() -> Self {
        Self {
            interval: Duration::from_secs(30),
            ..Default::default()
        }
    }
}

/// Errors from the poller.
#[derive(Debug, Error)]
pub enum PollError {
    #[error("client error: {0}")]
    Client(#[from] ClientError),
    #[error("auth error: {0}")]
    Auth(String),
}

/// One item emitted by the poller on its `items` channel. Lets the GUI
/// surface transient errors and auth failures in the UI rather than only
/// seeing successful event batches.
#[derive(Debug, Clone)]
pub enum PollItem {
    /// A successful poll produced these events.
    Events(Vec<RawEvent>),
    /// A poll failed (transient — server error, network, rate-limit). The
    /// poller keeps running; the UI can show a status line.
    Error(String),
    /// GitHub returned 401/403. Surfaced separately because it's terminal
    /// (the PAT is wrong or expired) and the UI should be loud about it.
    AuthError(String),
}

/// The result of a single poll cycle: events collected from sources that
/// succeeded, and the per-source errors from sources that didn't.
#[derive(Debug, Default)]
pub struct PollOutcome {
    pub events: Vec<RawEvent>,
    pub errors: Vec<PollError>,
}

/// A handle to a running poller. The poller runs in the background; you read
/// items from `items`. Drop the handle to stop it.
pub struct PollerHandle {
    join: tokio::task::JoinHandle<()>,
    pub items: mpsc::Receiver<PollItem>,
}

impl PollerHandle {
    /// Split the handle into its join handle and item receiver, so a
    /// consumer can hand the receiver to another task while keeping the
    /// join handle alive.
    pub fn into_parts(self) -> (tokio::task::JoinHandle<()>, mpsc::Receiver<PollItem>) {
        (self.join, self.items)
    }

    /// Stop the poller. Future `items.recv()` will return `None`.
    pub fn stop(self) {
        self.join.abort();
    }
}

/// The poller. Construct with `Poller::new` and start with `start`.
pub struct Poller {
    client: Client,
    cfg: PollConfig,
}

impl Poller {
    /// Build a poller. Validates auth.
    pub fn new(auth: Auth, cfg: PollConfig) -> Result<Self, PollError> {
        if cfg.username.is_none() && cfg.orgs.is_empty() && cfg.repos.is_empty() {
            return Err(PollError::Auth(
                "poller has no sources: set username, orgs, or repos".to_string(),
            ));
        }
        let client = Client::new(auth, ClientConfig::default())?;
        Ok(Self { client, cfg })
    }

    /// Build a poller with a custom client config (e.g. for testing).
    pub fn with_client_config(
        auth: Auth,
        cfg: PollConfig,
        client_cfg: ClientConfig,
    ) -> Result<Self, PollError> {
        let client = Client::new(auth, client_cfg)?;
        Ok(Self { client, cfg })
    }

    /// Start the poller in the background. Returns a handle for stopping it
    /// and a receiver for items (events + per-poll errors).
    pub fn start(self) -> PollerHandle {
        let (tx, rx) = mpsc::channel::<PollItem>(8);
        let join = tokio::spawn(async move {
            self.run(tx).await;
        });
        PollerHandle { join, items: rx }
    }

    async fn run(self, tx: mpsc::Sender<PollItem>) {
        info!(interval = ?self.cfg.interval, "poller started");
        let mut ticker = tokio::time::interval(self.cfg.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let outcome = self.poll_once().await;
            for err in &outcome.errors {
                let item = match err {
                    PollError::Client(ClientError::Unauthorized(msg)) => {
                        PollItem::AuthError(msg.clone())
                    }
                    _ => PollItem::Error(err.to_string()),
                };
                if tx.send(item).await.is_err() {
                    warn!("poller receiver dropped; stopping");
                    return;
                }
            }
            if !outcome.events.is_empty() {
                debug!(count = outcome.events.len(), "poll produced events");
                if tx.send(PollItem::Events(outcome.events)).await.is_err() {
                    warn!("poller receiver dropped; stopping");
                    return;
                }
            }
            if !outcome.errors.is_empty() {
                // back off briefly so we don't spin on persistent errors
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }

    /// Run a single poll cycle across all configured sources. Per-source
    /// failures are collected into the returned outcome rather than
    /// aborting the whole poll. Useful in tests.
    pub async fn poll_once(&self) -> PollOutcome {
        let mut out = PollOutcome::default();
        if let Some(user) = &self.cfg.username {
            match self.client.received_events(user).await {
                Ok(mut evs) => out.events.append(&mut evs),
                Err(e) => {
                    warn!(error = %e, "received_events failed");
                    out.errors.push(e.into());
                }
            }
        }
        for org in &self.cfg.orgs {
            match self.client.org_events(org).await {
                Ok(mut evs) => out.events.append(&mut evs),
                Err(e) => {
                    warn!(error = %e, org = %org, "org_events failed");
                    out.errors.push(e.into());
                }
            }
        }
        for full in &self.cfg.repos {
            if let Some((owner, repo)) = full.split_once('/') {
                match self.client.repo_events(owner, repo).await {
                    Ok(mut evs) => out.events.append(&mut evs),
                    Err(e) => {
                        warn!(error = %e, repo = %full, "repo_events failed");
                        out.errors.push(e.into());
                    }
                }
            } else {
                warn!(repo = %full, "repo must be in 'owner/name' form");
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_auth() -> Auth {
        Auth::new("ghp_test").unwrap()
    }

    #[tokio::test]
    async fn no_sources_is_error() {
        let cfg = PollConfig::default();
        assert!(Poller::new(test_auth(), cfg).is_err());
    }

    #[tokio::test]
    async fn poll_once_aggregates() {
        let server = MockServer::start().await;
        let body = r#"[{
            "id": "1",
            "type": "IssuesEvent",
            "created_at": "2026-06-13T10:00:00Z",
            "repo": {"name": "x/y"},
            "payload": {"action": "opened", "issue": {"title": "Bug"}}
        }]"#;
        Mock::given(method("GET"))
            .and(path("/users/octocat/received_events"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/orgs/rust-lang/events"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let cfg = PollConfig {
            username: Some("octocat".to_string()),
            orgs: vec!["rust-lang".to_string()],
            repos: vec![],
            interval: Duration::from_secs(60),
        };
        let poller = Poller::with_client_config(
            test_auth(),
            cfg,
            ClientConfig {
                base_url: server.uri(),
                user_agent: "test".to_string(),
                timeout: Duration::from_secs(5),
            },
        )
        .unwrap();
        let outcome = poller.poll_once().await;
        assert_eq!(outcome.events.len(), 2, "received + orgs");
        assert!(outcome.errors.is_empty());
    }

    #[tokio::test]
    async fn poll_once_surfaces_unauthorized_as_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let cfg = PollConfig {
            username: Some("octocat".to_string()),
            orgs: vec![],
            repos: vec![],
            interval: Duration::from_secs(60),
        };
        let poller = Poller::with_client_config(
            test_auth(),
            cfg,
            ClientConfig {
                base_url: server.uri(),
                user_agent: "test".to_string(),
                timeout: Duration::from_secs(5),
            },
        )
        .unwrap();
        let outcome = poller.poll_once().await;
        assert!(outcome.events.is_empty());
        assert_eq!(outcome.errors.len(), 1);
        assert!(matches!(
            outcome.errors[0],
            PollError::Client(ClientError::Unauthorized(_))
        ));
    }

    #[tokio::test]
    async fn poll_once_partial_failure_returns_events_and_errors() {
        let server = MockServer::start().await;
        // received_events succeeds.
        Mock::given(method("GET"))
            .and(path("/users/octocat/received_events"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"[{
                    "id": "1",
                    "type": "IssuesEvent",
                    "created_at": "2026-06-13T10:00:00Z",
                    "repo": {"name": "x/y"},
                    "payload": {"action": "opened", "issue": {"title": "Bug"}}
                }]"#,
            ))
            .mount(&server)
            .await;
        // org_events returns 500.
        Mock::given(method("GET"))
            .and(path("/orgs/rust-lang/events"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let cfg = PollConfig {
            username: Some("octocat".to_string()),
            orgs: vec!["rust-lang".to_string()],
            repos: vec![],
            interval: Duration::from_secs(60),
        };
        let poller = Poller::with_client_config(
            test_auth(),
            cfg,
            ClientConfig {
                base_url: server.uri(),
                user_agent: "test".to_string(),
                timeout: Duration::from_secs(5),
            },
        )
        .unwrap();
        let outcome = poller.poll_once().await;
        assert_eq!(outcome.events.len(), 1, "user source still works");
        assert_eq!(outcome.errors.len(), 1, "org source failed");
    }

    #[tokio::test]
    async fn start_and_stop() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
            .mount(&server)
            .await;
        let cfg = PollConfig {
            username: Some("octocat".to_string()),
            orgs: vec![],
            repos: vec![],
            interval: Duration::from_millis(50),
        };
        let poller = Poller::with_client_config(
            test_auth(),
            cfg,
            ClientConfig {
                base_url: server.uri(),
                user_agent: "test".to_string(),
                timeout: Duration::from_secs(5),
            },
        )
        .unwrap();
        let h = poller.start();
        // let it tick a few times
        tokio::time::sleep(Duration::from_millis(150)).await;
        h.stop();
    }
}
