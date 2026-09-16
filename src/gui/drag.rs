//! Moving the panel with the pointer.
//!
//! Pure arithmetic, deliberately separated from the toolkit for the same reason
//! [`crate::hotkey::geometry`] is separated from the Windows calls beneath it:
//! a window cannot be dragged inside a test, so the part that decides *where it
//! goes* has to live somewhere a test can reach.
//!
//! # Why the position is accumulated rather than read back
//!
//! The obvious implementation asks the toolkit where the window is each frame
//! and adds the pointer's movement to it. `ViewportInfo::outer_rect` is a frame
//! behind - it describes the window as it was when the last frame's input was
//! gathered, not as it is after the position this frame commanded - so adding a
//! delta to it double-counts, and the panel judders away from the pointer.
//!
//! What is stored instead is the position this module last *asked for*. The
//! pointer arrives in window-local coordinates, so "where the pointer is on the
//! desktop" is `outer + pointer`, which does not change when the window moves.
//! Holding that still under the grab point gives
//!
//! ```text
//! outer' = outer + (pointer - grab)
//! ```
//!
//! which settles exactly: once the window has caught up, `pointer == grab` and
//! the position stops changing; when the pointer moves by `d`, the window moves
//! by `d` and no more.
//!
//! # Why not `ViewportCommand::StartDrag`
//!
//! It is five lines and it hands the whole gesture to Windows, which is
//! genuinely tempting. It also enters the modal `WM_NCLBUTTONDOWN` move loop,
//! which blocks the event loop the panel's own animation is advanced from, and
//! leaves nothing behind that a test can hold this program to. The panel is the
//! window here - `gui::overlay::show` paints into `ui.max_rect()` - so a frozen
//! event loop is a frozen panel.

use eframe::egui::{Pos2, Vec2};

/// How far the pointer must travel before this is a move rather than a click.
///
/// Without it, a click that lands on empty chrome and wobbles by half a point
/// counts as a drag, writes a position to `%APPDATA%` and stops the panel ever
/// being placed again. Two points is below what a hand does by accident and far
/// below what one does on purpose.
pub const SLOP: f32 = 2.0;

/// Where the gesture started.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Anchor {
    /// Where in the window the pointer took hold, in points.
    grab: Pos2,
    /// The outer position last asked for, in points. See the module note.
    outer: Pos2,
}

/// A panel drag, in progress or not.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Drag {
    anchor: Option<Anchor>,
    /// Whether the pointer has travelled far enough for this to count.
    moved: bool,
}

impl Drag {
    pub const fn new() -> Self {
        Self {
            anchor: None,
            moved: false,
        }
    }

    /// Takes hold of the panel.
    ///
    /// `outer` is where the window is now; `grab` is where in it the pointer
    /// is. Both in points, because that is the unit egui reports and the unit
    /// `ViewportCommand::OuterPosition` expects - converting once at the
    /// boundary beats converting per field.
    pub fn begin(&mut self, grab: Pos2, outer: Pos2) {
        self.anchor = Some(Anchor { grab, outer });
        self.moved = false;
    }

    /// The position to command this frame.
    ///
    /// `None` when nothing is in flight, and also while the pointer is still
    /// inside [`SLOP`]: a window that shifts a point under a click is a window
    /// that feels loose.
    pub fn update(&mut self, pointer: Pos2) -> Option<Pos2> {
        let anchor = self.anchor.as_mut()?;
        let delta: Vec2 = pointer - anchor.grab;
        if !self.moved {
            if delta.length() < SLOP {
                return None;
            }
            self.moved = true;
        }
        anchor.outer += delta;
        Some(anchor.outer)
    }

    /// Lets go, and reports where the panel ended up.
    ///
    /// `None` when the gesture never became a move, which is the case a stray
    /// click has to fall into.
    pub fn end(&mut self) -> Option<Pos2> {
        let anchor = self.anchor.take()?;
        let moved = self.moved;
        self.moved = false;
        moved.then_some(anchor.outer)
    }

    /// Whether the panel is being moved right now.
    ///
    /// True only once the gesture has passed [`SLOP`], so a caller can use this
    /// to suppress a hover highlight without suppressing it under every click.
    pub fn is_dragging(&self) -> bool {
        self.anchor.is_some() && self.moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::pos2;

    /// The pointer moves by ten; so does the window, and not by twenty.
    #[test]
    fn the_window_follows_the_pointer_exactly() {
        let mut drag = Drag::new();
        drag.begin(pos2(100.0, 20.0), pos2(500.0, 300.0));

        let at = drag.update(pos2(110.0, 20.0)).expect("past the slop");
        assert_eq!(at, pos2(510.0, 300.0));
    }

    /// The frame after the window has caught up reports the pointer back at the
    /// grab point, and the position must then stand still. This is the whole
    /// reason the commanded position is accumulated rather than read back.
    #[test]
    fn a_window_that_has_caught_up_stops_moving() {
        let mut drag = Drag::new();
        drag.begin(pos2(100.0, 20.0), pos2(500.0, 300.0));

        assert_eq!(
            drag.update(pos2(140.0, 20.0)),
            Some(pos2(540.0, 300.0)),
            "the first frame moves by the pointer delta"
        );
        assert_eq!(
            drag.update(pos2(100.0, 20.0)),
            Some(pos2(540.0, 300.0)),
            "the window caught up, so the panel must stand still"
        );
        assert_eq!(drag.update(pos2(100.0, 20.0)), Some(pos2(540.0, 300.0)));
    }

    /// Three separate pushes add up to the sum of them, with no drift.
    #[test]
    fn successive_movements_accumulate() {
        let mut drag = Drag::new();
        drag.begin(pos2(0.0, 0.0), pos2(0.0, 0.0));

        drag.update(pos2(10.0, 0.0));
        drag.update(pos2(0.0, 10.0));
        let at = drag.update(pos2(0.0, 0.0));
        assert_eq!(at, Some(pos2(10.0, 10.0)));
    }

    /// A click that wobbles is a click. Without this the panel would write a
    /// position on every press and never be placed by `geometry::place` again.
    #[test]
    fn a_press_that_barely_moves_is_not_a_drag() {
        let mut drag = Drag::new();
        drag.begin(pos2(100.0, 20.0), pos2(500.0, 300.0));

        assert_eq!(drag.update(pos2(101.0, 20.5)), None);
        assert!(!drag.is_dragging());
        assert_eq!(drag.end(), None, "a wobble was reported as a move");
    }

    /// And once it is a drag it stays one, even if the pointer comes back
    /// through the grab point.
    #[test]
    fn a_drag_that_returns_to_where_it_started_is_still_a_drag() {
        let mut drag = Drag::new();
        drag.begin(pos2(100.0, 20.0), pos2(500.0, 300.0));

        drag.update(pos2(200.0, 20.0));
        assert!(drag.is_dragging());
        drag.update(pos2(0.0, 20.0));

        assert_eq!(
            drag.end(),
            Some(pos2(500.0, 300.0)),
            "the panel is back where it began, but the gesture happened"
        );
    }

    #[test]
    fn nothing_happens_without_a_grab() {
        let mut drag = Drag::new();
        assert_eq!(drag.update(pos2(10.0, 10.0)), None);
        assert_eq!(drag.end(), None);
        assert!(!drag.is_dragging());
    }

    /// Letting go and taking hold again starts a fresh gesture rather than
    /// resuming the last one.
    #[test]
    fn letting_go_clears_the_gesture() {
        let mut drag = Drag::new();
        drag.begin(pos2(0.0, 0.0), pos2(100.0, 100.0));
        drag.update(pos2(50.0, 0.0));
        assert_eq!(drag.end(), Some(pos2(150.0, 100.0)));

        assert!(!drag.is_dragging());
        drag.begin(pos2(0.0, 0.0), pos2(150.0, 100.0));
        assert!(!drag.is_dragging(), "the new gesture is not a move yet");
        assert_eq!(drag.update(pos2(10.0, 0.0)), Some(pos2(160.0, 100.0)));
    }
}
