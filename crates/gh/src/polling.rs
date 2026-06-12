//! The polling loop: repeatedly fetch events from all sources and emit them
//! on a tokio channel.

use std::future::Future;
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
/// seeing successful event batches. Each item carries a `&'static str`
/// source label so the GUI can track per-source status (e.g. `"received"`,
/// `"org/rust-lang"`, `"repo/octocat/Hello-World"`).
#[derive(Debug, Clone)]
pub enum PollItem {
    /// A source was polled successfully and produced these events (the
    /// vec may be empty).
    Events(&'static str, Vec<RawEvent>),
    /// A poll failed (transient — server error, network, rate-limit). The
    /// poller keeps running; the UI can show a status line.
    Error(&'static str, String),
    /// GitHub returned 401/403. Surfaced separately because it's terminal
    /// (the PAT is wrong or expired) and the UI should be loud about it.
    AuthError(&'static str, String),
}

/// Per-source outcome of a single poll cycle.
#[derive(Debug, Default)]
pub struct PollSourceOutcome {
    pub source: &'static str,
    pub events: Vec<RawEvent>,
    pub errors: Vec<PollError>,
}

/// The result of a single poll cycle: per-source outcomes. A source that
/// succeeded will have an empty `errors` vec; a source that failed will
/// have at least one entry in `errors` and an empty `events` vec.
#[derive(Debug, Default)]
pub struct PollOutcome {
    pub sources: Vec<PollSourceOutcome>,
}

impl PollOutcome {
    /// Total events across all sources.
    pub fn total_events(&self) -> usize {
        self.sources.iter().map(|s| s.events.len()).sum()
    }

    /// Total errors across all sources.
    pub fn total_errors(&self) -> usize {
        self.sources.iter().map(|s| s.errors.len()).sum()
    }
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
    /// Interned source labels, one per source in `cfg`, in poll order
    /// (received, then orgs, then repos). Used as the source field of
    /// every emitted `PollItem` and `PollSourceOutcome`. The labels are
    /// leaked at construction time; for a typical config (a handful of
    /// strings) the memory cost is negligible.
    sources: Vec<&'static str>,
}

impl Poller {
    /// Build a poller. Validates auth and interns the source labels.
    pub fn new(auth: Auth, cfg: PollConfig) -> Result<Self, PollError> {
        if cfg.username.is_none() && cfg.orgs.is_empty() && cfg.repos.is_empty() {
            return Err(PollError::Auth(
                "poller has no sources: set username, orgs, or repos".to_string(),
            ));
        }
        let client = Client::new(auth, ClientConfig::default())?;
        let sources = intern_sources(&cfg);
        Ok(Self {
            client,
            cfg,
            sources,
        })
    }

    /// Build a poller with a custom client config (e.g. for testing).
    pub fn with_client_config(
        auth: Auth,
        cfg: PollConfig,
        client_cfg: ClientConfig,
    ) -> Result<Self, PollError> {
        let client = Client::new(auth, client_cfg)?;
        let sources = intern_sources(&cfg);
        Ok(Self {
            client,
            cfg,
            sources,
        })
    }

    /// Start the poller in the background. Returns a handle for stopping it
    /// and a receiver for items (events + per-poll errors). The caller must
    /// be running on a tokio runtime (e.g. `#[tokio::test]` or Iced with the
    /// `tokio` feature enabled).
    pub fn start(self) -> PollerHandle {
        let (fut, items) = self.into_run();
        let join = tokio::spawn(fut);
        PollerHandle { join, items }
    }

    /// Decompose the poller into a future that runs the loop and a
    /// receiver for its items, without spawning. The caller decides which
    /// executor (if any) to spawn the future on. This is the v1 entry
    /// point for the Iced app, which provides its own tokio runtime.
    pub fn into_run(
        self,
    ) -> (
        impl Future<Output = ()> + Send + 'static,
        mpsc::Receiver<PollItem>,
    ) {
        let (tx, rx) = mpsc::channel::<PollItem>(8);
        let fut = self.run(tx);
        (fut, rx)
    }

    async fn run(self, tx: mpsc::Sender<PollItem>) {
        info!(interval = ?self.cfg.interval, "poller started");
        let mut ticker = tokio::time::interval(self.cfg.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let outcome = self.poll_once().await;
            let mut had_error = false;
            for source in outcome.sources {
                if !source.events.is_empty() {
                    debug!(
                        source = source.source,
                        count = source.events.len(),
                        "poll produced events"
                    );
                }
                for err in &source.errors {
                    had_error = true;
                    let item = match err {
                        PollError::Client(ClientError::Unauthorized(msg)) => {
                            PollItem::AuthError(source.source, msg.clone())
                        }
                        _ => PollItem::Error(source.source, err.to_string()),
                    };
                    if tx.send(item).await.is_err() {
                        warn!("poller receiver dropped; stopping");
                        return;
                    }
                }
                if tx
                    .send(PollItem::Events(source.source, source.events))
                    .await
                    .is_err()
                {
                    warn!("poller receiver dropped; stopping");
                    return;
                }
            }
            if had_error {
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
            let source = self.sources[0];
            let mut so = PollSourceOutcome {
                source,
                events: Vec::new(),
                errors: Vec::new(),
            };
            match self.client.received_events(user).await {
                Ok(mut evs) => so.events.append(&mut evs),
                Err(e) => {
                    warn!(error = %e, source = source, "received_events failed");
                    so.errors.push(e.into());
                }
            }
            out.sources.push(so);
        }
        let mut idx = if self.cfg.username.is_some() { 1 } else { 0 };
        for org in &self.cfg.orgs {
            let source = self.sources[idx];
            idx += 1;
            let mut so = PollSourceOutcome {
                source,
                events: Vec::new(),
                errors: Vec::new(),
            };
            match self.client.org_events(org).await {
                Ok(mut evs) => so.events.append(&mut evs),
                Err(e) => {
                    warn!(error = %e, source = source, "org_events failed");
                    so.errors.push(e.into());
                }
            }
            out.sources.push(so);
        }
        for full in &self.cfg.repos {
            if let Some((owner, repo)) = full.split_once('/') {
                let source = self.sources[idx];
                idx += 1;
                let mut so = PollSourceOutcome {
                    source,
                    events: Vec::new(),
                    errors: Vec::new(),
                };
                match self.client.repo_events(owner, repo).await {
                    Ok(mut evs) => so.events.append(&mut evs),
                    Err(e) => {
                        warn!(error = %e, source = source, "repo_events failed");
                        so.errors.push(e.into());
                    }
                }
                out.sources.push(so);
            } else {
                warn!(repo = %full, "repo must be in 'owner/name' form");
            }
        }
        out
    }
}

/// Leak the source labels so they live for `'static`. The number of
/// sources is bounded by the user's config (a handful), so the memory
/// cost is negligible.
fn intern_sources(cfg: &PollConfig) -> Vec<&'static str> {
    let mut out = Vec::new();
    if cfg.username.is_some() {
        out.push("received");
    }
    for org in &cfg.orgs {
        let label: &'static str = Box::leak(format!("org/{org}").into_boxed_str());
        out.push(label);
    }
    for repo in &cfg.repos {
        let label: &'static str = Box::leak(format!("repo/{repo}").into_boxed_str());
        out.push(label);
    }
    out
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
        assert_eq!(outcome.total_events(), 2, "received + orgs");
        assert_eq!(outcome.total_errors(), 0);
        assert_eq!(outcome.sources.len(), 2);
        assert_eq!(outcome.sources[0].source, "received");
        assert_eq!(outcome.sources[1].source, "org/rust-lang");
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
        assert_eq!(outcome.total_events(), 0);
        assert_eq!(outcome.total_errors(), 1);
        assert!(matches!(
            outcome.sources[0].errors[0],
            PollError::Client(ClientError::Unauthorized(_))
        ));
        assert_eq!(outcome.sources[0].source, "received");
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
        assert_eq!(outcome.total_events(), 1, "user source still works");
        assert_eq!(outcome.total_errors(), 1, "org source failed");
        // Sources are reported in the order they were polled; the user
        // source succeeded and the org source failed.
        let user = &outcome.sources[0];
        let org = &outcome.sources[1];
        assert_eq!(user.source, "received");
        assert_eq!(user.events.len(), 1);
        assert!(user.errors.is_empty());
        assert_eq!(org.source, "org/rust-lang");
        assert!(org.events.is_empty());
        assert_eq!(org.errors.len(), 1);
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

    #[tokio::test]
    async fn into_run_yields_receiver_and_runs() {
        // The Iced app uses `into_run` to get a future + receiver
        // without `Poller::start` doing the spawn for it. Verify that
        // spawning the future ourselves still produces items.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
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
        let (fut, mut rx) = poller.into_run();
        let _join = tokio::spawn(fut);
        // Wait for at least one item.
        let item = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("poller should produce an item within 2s")
            .expect("rx should not be closed");
        match item {
            PollItem::Events(source, evs) => {
                assert_eq!(source, "received");
                assert_eq!(evs.len(), 1);
            }
            other => panic!("expected Events, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_emits_per_source_items() {
        // One source succeeds, the other returns 500. The run loop should
        // emit one Events item for the successful source and one Error
        // item for the failing source, each tagged with the right label.
        let server = MockServer::start().await;
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
        Mock::given(method("GET"))
            .and(path("/orgs/rust-lang/events"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let cfg = PollConfig {
            username: Some("octocat".to_string()),
            orgs: vec!["rust-lang".to_string()],
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
        let (fut, mut rx) = poller.into_run();
        let _join = tokio::spawn(fut);
        // Collect the first cycle's items (expect: Events(received, [...]),
        // Error(org/rust-lang, ...), Events(org/rust-lang, []) — order
        // matches poll order).
        let mut events_received = None;
        let mut error_received = None;
        let mut org_empty_events = None;
        for _ in 0..3 {
            let item = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("poller should produce an item within 2s")
                .expect("rx should not be closed");
            match item {
                PollItem::Events("received", evs) if !evs.is_empty() => {
                    events_received = Some(evs.len());
                }
                PollItem::Error("org/rust-lang", _) => {
                    error_received = Some("org/rust-lang");
                }
                PollItem::Events("org/rust-lang", evs) => {
                    org_empty_events = Some(evs.is_empty());
                }
                other => panic!("unexpected item: {other:?}"),
            }
        }
        assert_eq!(events_received, Some(1));
        assert_eq!(error_received, Some("org/rust-lang"));
        assert_eq!(org_empty_events, Some(true));
    }
}
