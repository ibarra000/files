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
//! The price is worth paying for a transition somebody asked for. It was not
//! worth paying ten times per character typed, which is what was happening
//! before `on_input_changed` stopped emptying the result list - and what
//! [`Frame::resize`] below still guards against, by refusing to spend a round
//! trip on a size the display cannot tell apart from the current one.

use eframe::egui::{Vec2, vec2};

use crate::app::state::AppState;
use crate::gui::anim::{Motion, Phase, Target, Visual};
use crate::gui::overlay;
use crate::gui::theme::PANEL_W;

/// How far the window may sit from the size it wants before it is worth
/// telling the window system.
///
/// A whole point, against the half-point this replaces. Sizes are rounded to
/// points first, so this is really "do not re-send a size the panel is already
/// at": the tail of an ease-out spends most of its frames moving a fraction of
/// a point, and every one of those used to be a real round trip for a panel
/// that did not visibly move.
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
            busy: state.wants_animation(),
        });
        self.motion.advance(dt)
    }

    /// The size to ask the window system for, or `None` when it already has it.
    ///
    /// # Two regimes, because they are paying for different things
    ///
    /// **Arriving and leaving** the window is what moves, per frame, and that
    /// is the entrance: the edge, the surface and the content are one object
    /// rising into place. `dy` is spent on the last few points of height
    /// rather than on a translation so the panel grows rather than slides, and
    /// `scale` is spent on the width. It happens once per summon, and a
    /// hundred and forty milliseconds of resizes is what it costs.
    ///
    /// **While shown** the window goes to where the height is *heading*, not
    /// to where it is, and [`crate::gui::overlay::show`] paints the panel at
    /// `visual.height` inside it. So a list narrowing from eight rows to three
    /// is one resize with the surface easing down inside the window, rather
    /// than ten resizes with the window easing down around it - which on a
    /// frameless, always-on-top, per-pixel-alpha window is ten `SetWindowPos`
    /// calls and ten swapchain reconfigures, and looks like it.
    ///
    /// Growing is granted at once, because a window that arrives after its
    /// content would clip the footer for a tenth of a second. Shrinking waits
    /// for the tween to land, because a window that leaves before its content
    /// would clip it just as badly. The cost of that asymmetry is a strip of
    /// transparent window below the panel while it shrinks, which lasts as
    /// long as the transition and swallows a click nobody is making.
    ///
    /// `acrylic` turns all of it off. A compositor backdrop is a property of
    /// the *window*, so a window taller than the panel would show a blurred
    /// band with nothing in it - which is the failure the painted backdrop
    /// exists to avoid, arriving from the other side. The shipped
    /// configuration paints its own surface and never takes this path; it is
    /// here so that flipping `WANT_BACKDROP` stays a one-constant change.
    pub fn resize(&mut self, visual: &Visual, acrylic: bool) -> Option<Vec2> {
        let live = vec2(PANEL_W * visual.scale, (visual.height - visual.dy).max(1.0));
        let want = match self.motion.phase() {
            Phase::Shown if !acrylic => {
                let target = self.motion.height_target().max(1.0);
                let grew = self.window.y.is_nan() || target > self.window.y;
                if grew || self.motion.height_settled() {
                    vec2(PANEL_W, target)
                } else {
                    self.window
                }
            }
            _ => live,
        };

        // Rounded first, so the tail of an ease-out - which spends most of its
        // frames moving a fraction of a point - cannot spend a round trip on a
        // size the display cannot tell from the current one.
        let want = vec2(want.x.round(), want.y.round().max(1.0));
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
