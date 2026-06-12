//! `gh-monitor-config` — load and save user config.
//!
//! Stores the PAT, watched repos/orgs, and persisted window position.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod schema;

pub use schema::Config;
