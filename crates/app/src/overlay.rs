//! Hover-driven passthrough state machine.
//!
//! When the cursor is over the overlay, we capture input. When it leaves,
//! we let input pass through to whatever is underneath.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayState {
    /// Cursor is outside the overlay. Clicks pass through.
    Idle,
    /// Cursor is over the overlay. Clicks are captured.
    Active,
}

impl Default for OverlayState {
    fn default() -> Self {
        Self::Idle
    }
}

impl OverlayState {
    /// Whether the overlay should be in passthrough mode right now.
    pub fn is_passthrough(&self) -> bool {
        matches!(self, OverlayState::Idle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_is_passthrough() {
        assert!(OverlayState::Idle.is_passthrough());
        assert!(!OverlayState::Active.is_passthrough());
    }

    #[test]
    fn default_is_idle() {
        assert_eq!(OverlayState::default(), OverlayState::Idle);
    }
}
