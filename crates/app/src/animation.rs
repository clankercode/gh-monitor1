//! Per-node animation state. Each timeline node has an opacity animation
//! (fade-in on insert) and a pulse animation (transient on update).

use std::time::Instant;

use iced::animation::{Animation, Easing};
use std::time::Duration;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct NodeAnim {
    /// Opacity 0..1. Animated from 0 -> 1 on first appearance, then stays.
    pub opacity: Animation<f32>,
    /// Transient pulse 0..1, animated 0 -> 1 -> 0 over ~600ms on update.
    pub pulse: Animation<f32>,
    /// When this node first appeared.
    pub inserted_at: Instant,
    /// When this node was last updated (e.g. a new event folded into it).
    pub updated_at: Instant,
}

impl NodeAnim {
    /// Initial animation state for a brand-new node.
    pub fn new_insert(now: Instant) -> Self {
        let opacity = Animation::new(0.0_f32)
            .duration(Duration::from_millis(400))
            .easing(Easing::EaseOut);
        let mut opacity = opacity;
        opacity.go_mut(1.0, now);

        let pulse = Animation::new(0.0_f32)
            .duration(Duration::from_millis(600))
            .easing(Easing::EaseInOut);

        Self {
            opacity,
            pulse,
            inserted_at: now,
            updated_at: now,
        }
    }

    /// Trigger a pulse animation for an update.
    pub fn trigger_pulse(&mut self, now: Instant) {
        self.pulse.go_mut(1.0, now);
        self.updated_at = now;
    }

    /// Get the current opacity at `now`.
    pub fn opacity_at(&self, now: Instant) -> f32 {
        self.opacity.interpolate_with(|v| v, now)
    }

    /// Get the current pulse value at `now`.
    pub fn pulse_at(&self, now: Instant) -> f32 {
        self.pulse.interpolate_with(|v| v, now)
    }
}
