//! User config schema: PAT, watched repos/orgs, persisted window state.

use serde::{Deserialize, Serialize};

/// The full user config. Persisted to the platform's user config dir as
/// TOML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Config {
    /// Personal access token.
    pub pat: String,
    /// GitHub username (used to fetch `received_events`).
    pub username: Option<String>,
    /// Orgs to watch.
    #[serde(default)]
    pub orgs: Vec<String>,
    /// Repos to watch ("owner/name").
    #[serde(default)]
    pub repos: Vec<String>,
    /// Poll interval in seconds. Defaults to 30.
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    /// Last known window position.
    #[serde(default)]
    pub window_position: Option<WindowPosition>,
}

fn default_poll_interval() -> u64 {
    30
}

/// A window position (top-left, in physical pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowPosition {
    pub x: i32,
    pub y: i32,
}

impl Config {
    /// Validate the config. Returns Err if any required field is missing
    /// or malformed.
    pub fn validate(&self) -> Result<(), String> {
        if self.pat.trim().is_empty() {
            return Err("pat is empty".to_string());
        }
        if self.username.is_none() && self.orgs.is_empty() && self.repos.is_empty() {
            return Err("at least one of username, orgs, or repos must be set".to_string());
        }
        if self.poll_interval_secs < 5 {
            return Err("poll_interval_secs must be >= 5".to_string());
        }
        for r in &self.repos {
            if !r.contains('/') {
                return Err(format!("repo '{}' must be in 'owner/name' form", r));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_pat_fails() {
        let c = Config::default();
        assert!(c.validate().is_err());
    }

    #[test]
    fn no_sources_fails() {
        let c = Config {
            pat: "ghp_abc".to_string(),
            username: None,
            orgs: vec![],
            repos: vec![],
            poll_interval_secs: 30,
            window_position: None,
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn valid_username() {
        let c = Config {
            pat: "ghp_abc".to_string(),
            username: Some("octocat".to_string()),
            orgs: vec![],
            repos: vec![],
            poll_interval_secs: 30,
            window_position: None,
        };
        assert!(c.validate().is_ok());
    }

    #[test]
    fn bad_repo_format_fails() {
        let c = Config {
            pat: "ghp_abc".to_string(),
            username: None,
            orgs: vec![],
            repos: vec!["nope".to_string()],
            poll_interval_secs: 30,
            window_position: None,
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn roundtrip_toml() {
        let c = Config {
            pat: "ghp_abc".to_string(),
            username: Some("octocat".to_string()),
            orgs: vec!["rust-lang".to_string()],
            repos: vec!["octocat/Hello-World".to_string()],
            poll_interval_secs: 60,
            window_position: Some(WindowPosition { x: 100, y: 200 }),
        };
        let s = toml::to_string(&c).unwrap();
        let d: Config = toml::from_str(&s).unwrap();
        assert_eq!(c, d);
    }
}
