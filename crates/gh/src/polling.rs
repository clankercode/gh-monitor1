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

/// One item emitted by the poller on its `items` channel. A single
/// [`PollItem::Cycle`] is emitted per poll cycle, carrying events from
/// every source plus any per-source errors. Batching per cycle (rather
/// than per source) lets the GUI apply all events in one shot — if the
/// GUI rebuilt the snapshot per source, the last source polled would
/// "win" and the rest would flicker out.
///
/// Each entry carries a `&'static str` source label so the GUI can
/// track per-source status (e.g. `"received"`, `"org/rust-lang"`,
/// `"repo/octocat/Hello-World"`). Auth errors and transient errors are
/// both reported as `errors` — the message string tells them apart and
/// the GUI formats them the same way.
#[derive(Debug, Clone)]
pub enum PollItem {
    /// A full poll cycle completed. `events` lists every source that was
    /// polled (the per-source `Vec<RawEvent>` may be empty on a clean
    /// poll); `errors` lists sources that failed this cycle (transient
    /// or auth — same banner treatment).
    Cycle {
        events: Vec<(&'static str, Vec<RawEvent>)>,
        errors: Vec<(&'static str, String)>,
    },
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
            let mut events_out: Vec<(&'static str, Vec<RawEvent>)> =
                Vec::with_capacity(outcome.sources.len());
            let mut errors_out: Vec<(&'static str, String)> = Vec::new();
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
                    let msg = match err {
                        PollError::Client(ClientError::Unauthorized(msg)) => msg.clone(),
                        PollError::Client(c @ ClientError::RateLimited { .. }) => {
                            rate_limit_banner(c)
                        }
                        _ => err.to_string(),
                    };
                    errors_out.push((source.source, msg));
                }
                events_out.push((source.source, source.events));
            }
            if tx
                .send(PollItem::Cycle {
                    events: events_out,
                    errors: errors_out,
                })
                .await
                .is_err()
            {
                warn!("poller receiver dropped; stopping");
                return;
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

/// Format a `RateLimited` error for the user-facing status banner.
/// When the server supplied an `X-RateLimit-Reset` (or
/// `Retry-After`), the banner shows `rate-limited until <HH:MM:SS UTC>`
/// so the user knows when polling will resume. Without a reset time,
/// the banner falls back to the generic "rate-limited by GitHub".
pub(crate) fn rate_limit_banner(err: &ClientError) -> String {
    match err {
        ClientError::RateLimited { reset_at: Some(ts) } => {
            chrono::DateTime::from_timestamp(*ts as i64, 0)
                .map(|dt| format!("rate-limited until {}", dt.format("%Y-%m-%d %H:%M:%S UTC")))
                .unwrap_or_else(|| format!("rate-limited until epoch {ts}"))
        }
        ClientError::RateLimited { reset_at: None } => "rate-limited by GitHub".to_string(),
        // Defensive: only `RateLimited` is ever passed in, but keep a
        // sane fallback so this helper can be reused safely.
        other => other.to_string(),
    }
}

/// Leak the source labels so they live for `'static`. The number of
/// sources is bounded by the user's config (a handful), so the memory
/// cost is negligible.
///
/// Repos that aren't in `owner/name` form (e.g. `"nope"`, `"/x"`,
/// `"a/b/c"`) are dropped here. `Config::validate` already rejects
/// these but the poller is reachable in tests and from older configs;
/// skipping at the source keeps the `sources` vec's indices aligned
/// with `poll_once`'s `idx` counter.
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
        if repo
            .split_once('/')
            .filter(|(o, n)| !o.is_empty() && !n.is_empty() && !n.contains('/'))
            .is_none()
        {
            continue;
        }
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

    #[test]
    fn intern_sources_skips_malformed_repos() {
        // The poller's `intern_sources` and `poll_once` share a single
        // index space; if a malformed repo leaks into `intern_sources`
        // but is skipped in `poll_once` (or vice versa) the wrong
        // source label is bound to the wrong request. `Config::validate`
        // already rejects these, but the poller must also defend itself
        // so a stale or hand-edited config can't desync the labels.
        let cfg = PollConfig {
            username: Some("octocat".to_string()),
            orgs: vec!["rust-lang".to_string()],
            repos: vec![
                "nope".to_string(),
                "/leading".to_string(),
                "trailing/".to_string(),
                "a/b/c".to_string(),
                "good/one".to_string(),
            ],
            interval: Duration::from_secs(30),
        };
        let labels = intern_sources(&cfg);
        assert_eq!(
            labels,
            vec!["received", "org/rust-lang", "repo/good/one"],
            "malformed repos must be dropped"
        );
    }

    #[test]
    fn intern_sources_keeps_all_malformed_when_none_valid() {
        let cfg = PollConfig {
            username: None,
            orgs: vec![],
            repos: vec!["nope".to_string(), "/x".to_string(), "a/b/c".to_string()],
            interval: Duration::from_secs(30),
        };
        let labels = intern_sources(&cfg);
        assert!(labels.is_empty(), "expected no labels, got {labels:?}");
    }

    #[test]
    fn rate_limit_banner_with_reset_at_is_user_friendly() {
        // 1700000000 = 2023-11-14 22:13:20 UTC
        let err = ClientError::RateLimited {
            reset_at: Some(1700000000),
        };
        let msg = rate_limit_banner(&err);
        assert!(
            msg.starts_with("rate-limited until "),
            "unexpected banner: {msg}"
        );
        assert!(msg.contains("2023-11-14"), "missing date in {msg}");
        assert!(msg.contains("22:13:20"), "missing HH:MM:SS in {msg}");
        assert!(msg.contains("UTC"), "missing UTC in {msg}");
    }

    #[test]
    fn rate_limit_banner_without_reset_at_is_generic() {
        let err = ClientError::RateLimited { reset_at: None };
        let msg = rate_limit_banner(&err);
        assert_eq!(msg, "rate-limited by GitHub");
    }

    #[tokio::test]
    async fn rate_limit_429_with_reset_header_surfaces_user_message() {
        // End-to-end: a 429 with `X-RateLimit-Reset: 1234567890`
        // (which is 2009-02-13 23:31:30 UTC) must surface as
        // "rate-limited until 2009-02-13 23:31:30 UTC" in the per-source
        // error string, so the GUI banner shows the user when polling
        // will resume.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(429).insert_header("X-RateLimit-Reset", "1234567890"),
            )
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
        let raw = &outcome.sources[0].errors[0];
        let msg = match raw {
            PollError::Client(c @ ClientError::RateLimited { .. }) => rate_limit_banner(c),
            other => panic!("expected RateLimited, got {other:?}"),
        };
        assert!(
            msg.contains("rate-limited until"),
            "expected user-friendly banner, got: {msg}"
        );
        assert!(msg.contains("2009-02-13"), "missing date in {msg}");
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
            PollItem::Cycle { events, errors } => {
                assert!(errors.is_empty(), "no errors expected, got {errors:?}");
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].0, "received");
                assert_eq!(events[0].1.len(), 1);
            }
        }
    }

    #[tokio::test]
    async fn run_emits_one_cycle_per_tick() {
        // One source succeeds, the other returns 500. The run loop should
        // emit a single `PollItem::Cycle` per tick with both sources in
        // `events` (the failing one with an empty event vec) and the
        // failure in `errors`.
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
        let item = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("poller should produce a cycle within 2s")
            .expect("rx should not be closed");
        let PollItem::Cycle { events, errors } = item;
        assert_eq!(events.len(), 2, "both sources in events vec");
        let recv_evs: Vec<(&'static str, usize)> =
            events.iter().map(|(s, e)| (*s, e.len())).collect();
        assert!(recv_evs.contains(&("received", 1)));
        assert!(recv_evs.contains(&("org/rust-lang", 0)));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, "org/rust-lang");
        assert!(errors[0].1.contains("500"));
    }
}
