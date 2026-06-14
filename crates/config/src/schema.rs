//! User config schema: PAT, watched repos/orgs, persisted window state.

use serde::{Deserialize, Serialize};

/// Where to look for the GitHub personal access token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AuthSource {
    /// Use the PAT stored in the `pat` field.
    #[default]
    Pat,
    /// Use the GitHub CLI's stored credentials (reserved for future use).
    Gh,
}

/// The full user config. Persisted to the platform's user config dir as
/// TOML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Personal access token.
    pub pat: String,
    /// Where to source the token from.
    #[serde(default)]
    pub auth_source: AuthSource,
    /// GitHub username (used to fetch `received_events`).
    pub username: Option<String>,
    /// Orgs to watch.
    #[serde(default)]
    pub orgs: Vec<String>,
    /// Repos to watch ("owner/name").
    #[serde(default)]
    pub repos: Vec<String>,
    /// Poll interval in seconds. Defaults to 600 (10 minutes).
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    /// Whether desktop notifications are enabled.
    #[serde(default)]
    pub notifications_enabled: bool,
    /// Last known window position.
    #[serde(default)]
    pub window_position: Option<WindowPosition>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            pat: String::new(),
            auth_source: AuthSource::default(),
            username: None,
            orgs: Vec::new(),
            repos: Vec::new(),
            poll_interval_secs: default_poll_interval(),
            notifications_enabled: false,
            window_position: None,
        }
    }
}

fn default_poll_interval() -> u64 {
    600
}

/// A window position (top-left, in physical pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowPosition {
    /// X coordinate in physical pixels.
    pub x: i32,
    /// Y coordinate in physical pixels.
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
            let Some((owner, name)) = r.split_once('/') else {
                return Err(format!("repo '{}' must be in 'owner/name' form", r));
            };
            if owner.is_empty() || name.is_empty() || name.contains('/') {
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
            ..Config::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn valid_username() {
        let c = Config {
            pat: "ghp_abc".to_string(),
            username: Some("octocat".to_string()),
            ..Config::default()
        };
        assert!(c.validate().is_ok());
    }

    #[test]
    fn bad_repo_format_fails() {
        let c = Config {
            pat: "ghp_abc".to_string(),
            repos: vec!["nope".to_string()],
            ..Config::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn repo_leading_slash_fails() {
        // `"/x"` parses (it contains a `/`) but owner is empty.
        let c = Config {
            pat: "ghp_abc".to_string(),
            repos: vec!["/x".to_string()],
            ..Config::default()
        };
        let err = c.validate().unwrap_err();
        assert!(err.contains("owner/name"), "unexpected error: {err}");
    }

    #[test]
    fn repo_trailing_slash_fails() {
        // `"x/"` parses (it contains a `/`) but name is empty.
        let c = Config {
            pat: "ghp_abc".to_string(),
            repos: vec!["x/".to_string()],
            ..Config::default()
        };
        let err = c.validate().unwrap_err();
        assert!(err.contains("owner/name"), "unexpected error: {err}");
    }

    #[test]
    fn repo_too_many_slashes_fails() {
        // `"a/b/c"` has more than one `/` — name contains `/`.
        let c = Config {
            pat: "ghp_abc".to_string(),
            repos: vec!["a/b/c".to_string()],
            ..Config::default()
        };
        let err = c.validate().unwrap_err();
        assert!(err.contains("owner/name"), "unexpected error: {err}");
    }

    #[test]
    fn repo_minimal_form_ok() {
        let c = Config {
            pat: "ghp_abc".to_string(),
            repos: vec!["a/b".to_string()],
            ..Config::default()
        };
        assert!(c.validate().is_ok());
    }

    #[test]
    fn repo_realistic_form_ok() {
        let c = Config {
            pat: "ghp_abc".to_string(),
            repos: vec!["rust-lang/rust".to_string()],
            ..Config::default()
        };
        assert!(c.validate().is_ok());
    }

    #[test]
    fn auth_source_defaults_to_pat() {
        let c = Config::default();
        assert_eq!(c.auth_source, AuthSource::Pat);
    }

    #[test]
    fn auth_source_roundtrips_via_toml() {
        let toml_str = r#"
            pat = "ghp_abc"
            auth_source = "gh"
            username = "octocat"
        "#;
        let c: Config = toml::from_str(toml_str).expect("parse");
        assert_eq!(c.auth_source, AuthSource::Gh);
        let back = toml::to_string(&c).unwrap();
        let c2: Config = toml::from_str(&back).unwrap();
        assert_eq!(c2.auth_source, AuthSource::Gh);
    }

    #[test]
    fn notifications_enabled_defaults_to_false() {
        let c = Config::default();
        assert!(!c.notifications_enabled);
    }

    #[test]
    fn roundtrip_toml() {
        let c = Config {
            pat: "ghp_abc".to_string(),
            auth_source: AuthSource::Gh,
            username: Some("octocat".to_string()),
            orgs: vec!["rust-lang".to_string()],
            repos: vec!["octocat/Hello-World".to_string()],
            poll_interval_secs: 60,
            notifications_enabled: true,
            window_position: Some(WindowPosition { x: 100, y: 200 }),
        };
        let s = toml::to_string(&c).unwrap();
        let d: Config = toml::from_str(&s).unwrap();
        assert_eq!(c, d);
    }
}
