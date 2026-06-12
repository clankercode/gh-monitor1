//! Authentication: personal access token.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// GitHub authentication. We use PATs only in v1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Auth {
    /// The raw PAT.
    pub pat: String,
}

impl Auth {
    /// Create a new Auth from a raw PAT. Validates that it's non-empty.
    pub fn new(pat: impl Into<String>) -> Result<Self, AuthError> {
        let pat = pat.into();
        if pat.trim().is_empty() {
            return Err(AuthError::Empty);
        }
        Ok(Self { pat })
    }

    /// The Authorization header value to send to GitHub.
    pub fn header_value(&self) -> String {
        format!("Bearer {}", self.pat)
    }
}

/// Errors when constructing an [`Auth`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("personal access token is empty")]
    Empty,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_pat_rejected() {
        assert!(Auth::new("").is_err());
        assert!(Auth::new("   ").is_err());
    }

    #[test]
    fn header_value() {
        let a = Auth::new("ghp_abc").unwrap();
        assert_eq!(a.header_value(), "Bearer ghp_abc");
    }
}
