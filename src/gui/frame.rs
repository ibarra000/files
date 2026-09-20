//! One frame's composition.
//!
//! The three steps `Shell::ui` takes before it draws anything: measure the
//! state, tell the animator, ask it what this frame looks like.
//!
//! Lifted out of the shell because all three are pure. Nothing here opens a
//! window, reads a clock or touches the GPU: it is `(&AppState, dt)` in and a
//! [`Visual`] out, exactly the bargain [`crate::gui::anim::Motion`] already
//! makes one layer down. That is what lets a test drive the real composition
//! rather than a transcription of it.
//!
//! # There used to be a fourth step
//!
//! Deciding how big the window should be. `overlay::show` paints into
//! `ui.max_rect()`, so the window's height *was* the panel's height, and
//! resizing it was how the panel grew and shrank. That cost a `SetWindowPos`
//! and a swapchain reconfigure per change, which is why the height transition
//! was deleted, why a dead-band guarded the rounding, and why
//! `tests/jitter.rs` counted resizes at all.
//!
//! The panel is six hundred by four hundred now and stays there, so the whole
//! question is answered by a constant and none of that machinery has anything
//! left to do. The list scrolls inside the window instead of the window
//! growing around the list.

use crate::app::state::AppState;
use crate::gui::anim::{Motion, Target, Visual};
use crate::gui::overlay;

/// The panel's motion.
#[derive(Default)]
pub struct Frame {
    pub motion: Motion,
}

impl Frame {
    pub fn new() -> Self {
        Self::default()
    }

    /// Told what the world looks like, then how much time has passed, then
    /// asked what to draw. One order, in one place.
    pub fn advance(&mut self, state: &AppState, dt: f32) -> Visual {
        let measured = overlay::measure(state);
        self.motion.retarget(Target {
            content: measured.content,
            selection_y: measured.selection_y,
        });
        self.motion.advance(dt)
    }
}
