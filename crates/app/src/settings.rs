//! Pure-logic settings form: the editable mirror of [`Config`].
//!
//! Lives in `crates/app` because it depends on the schema (no GUI, no
//! Iced). The GUI in `app.rs` renders the form as a column of Iced
//! `TextInput`s, fires [`SettingsFieldChanged`] on every keystroke, and
//! on Save calls [`SettingsForm::to_config`] to build a validated
//! `Config` and persist it via `save_config`.
//!
//! All mutations go through [`SettingsForm::update_field`], which keeps
//! the form state in one place and the GUI in lockstep with it.

use gh_monitor_config::schema::AuthSource;
use gh_monitor_config::Config;

/// An editable field in the settings form. Used as the discriminant for
/// [`SettingsForm::update_field`] so the GUI can route any keystroke to
/// the right field without keeping per-field message variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsField {
    /// Personal access token.
    Pat,
    /// Auth source (`Pat` | `Gh`).
    AuthSource,
    /// GitHub username.
    Username,
    /// Comma-separated orgs.
    Orgs,
    /// Comma-separated repos (`owner/name`).
    Repos,
    /// Poll interval in seconds.
    PollInterval,
    /// System notifications on new activity.
    Notifications,
}

/// The settings form's working copy. Mirrors the editable subset of
/// [`Config`]. Fields are stored as `String` so they can be bound
/// directly to Iced `TextInput`s without further conversion. Numeric /
/// enum fields are parsed only at Save time, via [`to_config`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsForm {
    pat: String,
    auth_source: AuthSource,
    username: String,
    orgs: String,
    repos: String,
    poll_interval_secs: String,
    notifications_enabled: bool,
}

impl SettingsForm {
    /// Build a form initialised from `cfg`. The current values are the
    /// in-memory form, so a Cancel returns to the loaded config.
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            pat: cfg.pat.clone(),
            auth_source: cfg.auth_source,
            username: cfg.username.clone().unwrap_or_default(),
            orgs: cfg.orgs.join(", "),
            repos: cfg.repos.join(", "),
            poll_interval_secs: cfg.poll_interval_secs.to_string(),
            notifications_enabled: cfg.notifications_enabled,
        }
    }

    /// Build a form from the default config. Used by the in-pane
    /// "Reset to defaults" button.
    pub fn from_default() -> Self {
        Self::from_config(&Config::default())
    }

    /// Validate the form and build a [`Config`]. `window_position` is
    /// taken from the live `current` config so a Save does not lose
    /// where the user has dragged the overlay to.
    pub fn to_config(&self, current: &Config) -> Result<Config, String> {
        let pat = self.pat.trim().to_string();
        let username_raw = self.username.trim().to_string();
        let username = if username_raw.is_empty() {
            None
        } else {
            Some(username_raw)
        };
        let orgs = parse_list(&self.orgs);
        let repos = parse_list(&self.repos);
        let poll_interval_secs: u64 = self.poll_interval_secs.trim().parse().map_err(|_| {
            format!(
                "poll interval must be a whole number of seconds (got {:?})",
                self.poll_interval_secs
            )
        })?;
        let cfg = Config {
            pat,
            auth_source: self.auth_source,
            username,
            orgs,
            repos,
            poll_interval_secs,
            notifications_enabled: self.notifications_enabled,
            window_position: current.window_position,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    /// Pure update: replace the value of `field` with `value`. Strings
    /// are stored verbatim (no trimming); the only coercion is on the
    /// enum / bool / numeric variants. Returns `false` if the value
    /// didn't actually change (so the caller can short-circuit any
    /// re-render or save).
    pub fn update_field(&mut self, field: SettingsField, value: String) -> bool {
        match field {
            SettingsField::Pat => set_if_changed(&mut self.pat, value),
            SettingsField::AuthSource => {
                let parsed = match value.as_str() {
                    "gh" => AuthSource::Gh,
                    _ => AuthSource::Pat,
                };
                if parsed == self.auth_source {
                    false
                } else {
                    self.auth_source = parsed;
                    true
                }
            }
            SettingsField::Username => set_if_changed(&mut self.username, value),
            SettingsField::Orgs => set_if_changed(&mut self.orgs, value),
            SettingsField::Repos => set_if_changed(&mut self.repos, value),
            SettingsField::PollInterval => set_if_changed(&mut self.poll_interval_secs, value),
            SettingsField::Notifications => {
                let parsed = matches!(value.as_str(), "true" | "1" | "on");
                if parsed == self.notifications_enabled {
                    false
                } else {
                    self.notifications_enabled = parsed;
                    true
                }
            }
        }
    }

    pub fn pat(&self) -> &str {
        &self.pat
    }
    pub fn auth_source(&self) -> AuthSource {
        self.auth_source
    }
    pub fn username(&self) -> &str {
        &self.username
    }
    pub fn orgs(&self) -> &str {
        &self.orgs
    }
    pub fn repos(&self) -> &str {
        &self.repos
    }
    pub fn poll_interval_secs(&self) -> &str {
        &self.poll_interval_secs
    }
    pub fn notifications_enabled(&self) -> bool {
        self.notifications_enabled
    }
}

fn set_if_changed(slot: &mut String, value: String) -> bool {
    if *slot == value {
        false
    } else {
        *slot = value;
        true
    }
}

/// Parse a comma-separated list, trimming whitespace and dropping
/// empties. Mirrors `setup::parse_list` so the form behaves like the
/// CLI wizard for org / repo input.
fn parse_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gh_monitor_config::schema::WindowPosition;

    fn sample() -> Config {
        Config {
            pat: "ghp_test".to_string(),
            auth_source: AuthSource::Pat,
            username: Some("octocat".to_string()),
            orgs: vec!["rust-lang".to_string(), "tokio-rs".to_string()],
            repos: vec!["octocat/Hello-World".to_string()],
            poll_interval_secs: 600,
            notifications_enabled: true,
            window_position: Some(WindowPosition { x: 10, y: 20 }),
        }
    }

    #[test]
    fn settings_form_from_config_roundtrips() {
        let cfg = sample();
        let form = SettingsForm::from_config(&cfg);
        let back = form.to_config(&cfg).expect("form should validate");
        assert_eq!(back.pat, cfg.pat);
        assert_eq!(back.auth_source, cfg.auth_source);
        assert_eq!(back.username, cfg.username);
        assert_eq!(back.orgs, cfg.orgs);
        assert_eq!(back.repos, cfg.repos);
        assert_eq!(back.poll_interval_secs, cfg.poll_interval_secs);
        assert_eq!(back.notifications_enabled, cfg.notifications_enabled);
        assert_eq!(back.window_position, cfg.window_position);
    }

    #[test]
    fn settings_form_to_config_rejects_empty_pat() {
        let cfg = sample();
        let mut form = SettingsForm::from_config(&cfg);
        form.update_field(SettingsField::Pat, String::new());
        let err = form.to_config(&cfg).unwrap_err();
        assert!(err.contains("pat"), "unexpected error: {err}");
    }

    #[test]
    fn settings_form_to_config_rejects_whitespace_pat() {
        let cfg = sample();
        let mut form = SettingsForm::from_config(&cfg);
        form.update_field(SettingsField::Pat, "   ".to_string());
        assert!(form.to_config(&cfg).is_err());
    }

    #[test]
    fn settings_form_to_config_rejects_malformed_repo() {
        let cfg = sample();
        let mut form = SettingsForm::from_config(&cfg);
        form.update_field(SettingsField::Repos, "not-a-slash".to_string());
        let err = form.to_config(&cfg).unwrap_err();
        assert!(err.contains("owner/name"), "unexpected error: {err}");
    }

    #[test]
    fn settings_form_to_config_rejects_non_numeric_poll_interval() {
        let cfg = sample();
        let mut form = SettingsForm::from_config(&cfg);
        form.update_field(SettingsField::PollInterval, "abc".to_string());
        let err = form.to_config(&cfg).unwrap_err();
        assert!(err.contains("whole number"), "unexpected error: {err}");
    }

    #[test]
    fn settings_form_to_config_rejects_low_poll_interval() {
        let cfg = sample();
        let mut form = SettingsForm::from_config(&cfg);
        form.update_field(SettingsField::PollInterval, "2".to_string());
        let err = form.to_config(&cfg).unwrap_err();
        assert!(err.contains("5"), "unexpected error: {err}");
    }

    #[test]
    fn settings_form_to_config_rejects_missing_sources() {
        let cfg = sample();
        let form = SettingsForm {
            pat: "ghp_test".to_string(),
            auth_source: AuthSource::Pat,
            username: String::new(),
            orgs: String::new(),
            repos: String::new(),
            poll_interval_secs: "600".to_string(),
            notifications_enabled: false,
        };
        let err = form.to_config(&cfg).unwrap_err();
        assert!(
            err.contains("username") || err.contains("orgs") || err.contains("repos"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn settings_form_update_field_changes_value() {
        let cfg = sample();
        let mut form = SettingsForm::from_config(&cfg);
        let changed = form.update_field(SettingsField::Pat, "ghp_new".to_string());
        assert!(changed);
        assert_eq!(form.pat(), "ghp_new");
    }

    #[test]
    fn settings_form_update_field_no_change_returns_false() {
        let cfg = sample();
        let mut form = SettingsForm::from_config(&cfg);
        let changed = form.update_field(SettingsField::Pat, cfg.pat.clone());
        assert!(!changed, "identical value should report no change");
    }

    #[test]
    fn settings_form_update_field_auth_source_parses() {
        let cfg = sample();
        let mut form = SettingsForm::from_config(&cfg);
        form.update_field(SettingsField::AuthSource, "gh".to_string());
        assert_eq!(form.auth_source(), AuthSource::Gh);
        form.update_field(SettingsField::AuthSource, "pat".to_string());
        assert_eq!(form.auth_source(), AuthSource::Pat);
    }

    #[test]
    fn settings_form_update_field_notifications_parses_truthy() {
        let cfg = sample();
        let mut form = SettingsForm::from_config(&cfg);
        form.update_field(SettingsField::Notifications, "true".to_string());
        assert!(form.notifications_enabled());
        form.update_field(SettingsField::Notifications, "false".to_string());
        assert!(!form.notifications_enabled());
        form.update_field(SettingsField::Notifications, "on".to_string());
        assert!(form.notifications_enabled());
    }

    #[test]
    fn settings_form_update_field_lists_keep_whitespace_layout() {
        let cfg = sample();
        let mut form = SettingsForm::from_config(&cfg);
        form.update_field(SettingsField::Orgs, " a , b ,  c ".to_string());
        assert_eq!(form.orgs(), " a , b ,  c ");
        let back = form.to_config(&cfg).expect("valid");
        assert_eq!(
            back.orgs,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn settings_form_default_matches_config_default() {
        let form = SettingsForm::from_default();
        assert_eq!(form.pat(), "");
        assert_eq!(form.auth_source(), AuthSource::default());
        assert_eq!(form.username(), "");
        assert_eq!(form.orgs(), "");
        assert_eq!(form.repos(), "");
        assert_eq!(form.poll_interval_secs(), "600");
        assert!(!form.notifications_enabled());
    }

    #[test]
    fn settings_form_default_roundtrips_via_config_default() {
        let form = SettingsForm::from_default();
        let cfg = Config::default();
        assert_eq!(form.pat(), cfg.pat);
        assert_eq!(form.username(), cfg.username.as_deref().unwrap_or(""));
        assert_eq!(form.orgs(), cfg.orgs.join(", "));
        assert_eq!(form.repos(), cfg.repos.join(", "));
        assert_eq!(
            form.poll_interval_secs(),
            cfg.poll_interval_secs.to_string()
        );
        assert_eq!(form.notifications_enabled(), cfg.notifications_enabled);
    }

    #[test]
    fn settings_form_to_config_preserves_window_position() {
        let mut cfg = sample();
        cfg.window_position = Some(WindowPosition { x: 123, y: 456 });
        let form = SettingsForm::from_config(&cfg);
        let back = form.to_config(&cfg).expect("valid");
        assert_eq!(
            back.window_position,
            Some(WindowPosition { x: 123, y: 456 })
        );
    }
}
