//! `gh-monitor-gh` — GitHub REST client + polling loop.
//!
//! Pure logic. No GUI, no Iced. Returns a stream of `RawEvent`s that the
//! timeline crate groups and the app crate renders.
//!
//! Modules:
//! - [`auth`] — personal access token handling
//! - [`events`] — GitHub event types (PR opened, PR merged, issue opened,
//!   release published, new repo created) and parsing from the REST API
//! - [`client`] — reqwest wrapper with auth, rate-limit handling, and retry
//! - [`polling`] — poll loop that yields `RawEvent`s via a tokio channel

#![forbid(unsafe_code)]
#![allow(missing_docs)]

pub mod auth;
pub mod client;
pub mod events;
pub mod polling;

pub use auth::Auth;
pub use client::{Client, ClientConfig, ClientError};
pub use events::{EventKind, RawEvent};
pub use polling::{rate_limit_banner, PollConfig, PollError, PollItem, Poller, PollerHandle};
