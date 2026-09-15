//! One frame's composition, and the decision about the window's size.
//!
//! The three steps `Shell::ui` takes before it draws anything - measure the
//! state, tell the animator, ask it what this frame looks like - plus the one
//! it takes after: decide whether the window system needs to hear about it.
//!
//! Lifted out of the shell because all four are pure. Nothing here opens a
//! window, reads a clock or touches the GPU: it is `(&AppState, dt)` in and a
//! [`Visual`] out, exactly the bargain [`crate::gui::anim::Motion`] already
//! makes one layer down. That is what lets a test drive the real composition
//! rather than a transcription of it, and it is the only way the jitter this
//! module exists to keep out is checkable at all - a panel that flinches once
//! per keystroke is not something anybody notices in a screenshot.
//!
//! # Why the window is what moves
//!
//! [`crate::gui::overlay::show`] paints into `ui.max_rect()` - the whole
//! window - so the window's height *is* the panel's height, and resizing it is
//! how the panel grows and shrinks. That is deliberate (see the note on
//! `Shell::resize`'s ancestor), and it has a price: every frame of a height
//! transition is a `SetWindowPos` and a swapchain reconfigure.
//!
//! That price is why there is no longer a height transition to pay it for.
//! Ten resizes per character was the first version; one per genuine row-count
//! change was the second; and with the search debounced, a typed code produces
//! one row-count change and therefore one resize. What is left in
//! [`Frame::resize`] below is the rounding guard, which refuses to spend a
//! round trip on a size the display cannot tell apart from the current one.

use eframe::egui::{Vec2, vec2};

use crate::app::state::AppState;
use crate::gui::anim::{Motion, Target, Visual};
use crate::gui::overlay;
use crate::gui::theme::PANEL_W;

/// How far the window may sit from the size it wants before it is worth
/// telling the window system.
///
/// Sized originally against the tail of an ease-out, which spent most of its
/// frames moving a fraction of a point. There is no ease-out now, and
/// `overlay::measure` produces exact multiples of `ROW_H` plus constants - so
/// consecutive heights differ by forty points or by nothing, and this is left
/// as the guard against re-sending a size the window already has.
const DEADBAND: f32 = 1.0;

/// The panel's motion, and the size the window was last asked for.
pub struct Frame {
    pub motion: Motion,
    window: Vec2,
}

impl Default for Frame {
    fn default() -> Self {
        Self::new()
    }
}

impl Frame {
    pub fn new() -> Self {
        Self {
            motion: Motion::new(),
            // Deliberately not `ZERO`: the first real size must always be
            // sent, and no size is within the dead-band of NaN.
            window: vec2(f32::NAN, f32::NAN),
        }
    }

    /// Told what the world looks like, then how much time has passed, then
    /// asked what to draw. One order, in one place.
    pub fn advance(&mut self, state: &AppState, dt: f32) -> Visual {
        let measured = overlay::measure(state);
        self.motion.retarget(Target {
            height: measured.height,
            content: measured.content,
            selection_y: measured.selection_y,
        });
        self.motion.advance(dt)
    }

    /// The size to ask the window system for, or `None` when it already has it.
    ///
    /// The window's height *is* the panel's height (see the module note), so
    /// this is the whole of the resize policy. It used to have two regimes and
    /// an asymmetry between growing and shrinking, because a height tween meant
    /// the window and its content were at different heights for a tenth of a
    /// second and something had to decide which of them was right. Without the
    /// tween they are the same number, and there is nothing left to arbitrate.
    ///
    /// They still disagree for exactly one frame, because a viewport command is
    /// a round trip and the frame that asks is drawn before it lands. That is
    /// what the clamp in [`crate::gui::overlay::panel_rect`] is for; an
    /// asymmetry cannot help with one frame, and both directions are gone
    /// before a monitor has drawn them twice.
    pub fn resize(&mut self, visual: &Visual) -> Option<Vec2> {
        // Rounded first. Sizes are sent in points and a display cannot tell
        // 320.0 from 320.4 apart, so `DEADBAND` is now purely a guard against
        // re-sending a size the window is already at.
        let want = vec2(PANEL_W.round(), visual.height.round().max(1.0));
        if (want - self.window).length() < DEADBAND {
            return None;
        }
        self.window = want;
        Some(want)
    }

    /// The size last asked for. `None` before the first frame.
    pub fn window(&self) -> Option<Vec2> {
        self.window.is_finite().then_some(self.window)
    }
}
