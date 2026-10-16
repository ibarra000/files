//! Per-frame composition for `Shell::ui`: measure the state, update the
//! motion, and return the body to draw.
//!
//! Pure (`(&AppState, dt)` in, [`Content`] out), so tests can drive the real
//! composition. The window has a fixed size; the list scrolls inside it.

use crate::app::state::AppState;
use crate::gui::anim::{Content, Motion};
use crate::gui::panel;

/// The panel's motion.
#[derive(Default)]
pub struct Frame {
    pub motion: Motion,
}

impl Frame {
    pub fn new() -> Self {
        Self::default()
    }

    /// Retargets the motion from `state`, advances it by `dt`, and returns the
    /// body to draw.
    pub fn advance(&mut self, state: &AppState, dt: f32) -> Content {
        self.motion.retarget(panel::body_of(state));
        self.motion.advance(dt)
    }
}
