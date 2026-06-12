//! The polling loop: repeatedly fetch events from all sources and emit them
//! on a tokio channel.

use std::time::Duration;

use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

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

/// A handle to a running poller. The poller runs in the background; you read
/// events from `events`. Drop the handle to stop it.
pub struct PollerHandle {
    join: tokio::task::JoinHandle<()>,
    pub events: mpsc::Receiver<Vec<RawEvent>>,
}

impl PollerHandle {
    /// Stop the poller. Future `events.recv()` will return `None`.
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
    /// and a receiver for events. Each poll produces one `Vec<RawEvent>`
    /// batch on the channel.
    pub fn start(self) -> PollerHandle {
        let (tx, rx) = mpsc::channel::<Vec<RawEvent>>(8);
        let join = tokio::spawn(async move {
            self.run(tx).await;
        });
        PollerHandle { join, events: rx }
    }

    async fn run(self, tx: mpsc::Sender<Vec<RawEvent>>) {
        info!(interval = ?self.cfg.interval, "poller started");
        let mut ticker = tokio::time::interval(self.cfg.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            match self.poll_once().await {
                Ok(events) => {
                    debug!(count = events.len(), "poll produced events");
                    if !events.is_empty()
                        && tx.send(events).await.is_err()
                    {
                        warn!("poller receiver dropped; stopping");
                        break;
                    }
                }
                Err(e) => {
                    error!(error = %e, "poll failed");
                    // back off: wait a few seconds before next attempt
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }

    /// Run a single poll cycle across all configured sources. Useful in tests.
    pub async fn poll_once(&self) -> Result<Vec<RawEvent>, PollError> {
        let mut out = Vec::new();
        if let Some(user) = &self.cfg.username {
            match self.client.received_events(user).await {
                Ok(mut evs) => out.append(&mut evs),
                Err(e) => warn!(error = %e, "received_events failed"),
            }
        }
        for org in &self.cfg.orgs {
            match self.client.org_events(org).await {
                Ok(mut evs) => out.append(&mut evs),
                Err(e) => warn!(error = %e, org = %org, "org_events failed"),
            }
        }
        for full in &self.cfg.repos {
            if let Some((owner, repo)) = full.split_once('/') {
                match self.client.repo_events(owner, repo).await {
                    Ok(mut evs) => out.append(&mut evs),
                    Err(e) => warn!(error = %e, repo = %full, "repo_events failed"),
                }
            } else {
                warn!(repo = %full, "repo must be in 'owner/name' form");
            }
        }
        Ok(out)
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
        let events = poller.poll_once().await.unwrap();
        assert_eq!(events.len(), 2, "received + orgs");
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
